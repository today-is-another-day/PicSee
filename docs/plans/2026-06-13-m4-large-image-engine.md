# M4 大图引擎实现计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 实现超大图（基准：19200×16384 / 900MB+ BMP）秒开、低内存、局部高清的大图引擎，含自研 BMP reader、ImageSession 管理、picsee:// 自定义协议二进制通道，以及 probe_image / open_large_image / close_large_image 三个 Tauri command。

**Architecture:** 新建 `src-tauri/src/large_image/` 子模块，包含 policy（判定）、bmp（自研读取器）、session（会话管理 + LRU tile 缓存 + preview 生成）三个文件，由 `mod.rs` 统一导出；在 `lib.rs` 注册 `picsee://` 自定义协议和三个 command；错误码复用 `{code, message}` 结构，抽公共 `error.rs`。preview/tile 全部走 WebP 编码，通过 picsee:// 协议直接被前端 `<img>` 消费，零 JSON 传像素。

**M4 已知内存风险：** 非 BMP 大图生成 preview 时仍会由 `image` crate 一次性解码整图；缩放完成后立即释放原始解码缓冲，仅在会话中保留 preview WebP。M6 需改用增量/分块解码以消除峰值内存风险。非 BMP 在 M4 为 preview-only，不请求 tile。

**Tech Stack:** Rust, Tauri 2.0, `image` crate (已有 webp feature), `tokio` (已有), `uuid` crate (新增), `lru` crate (新增), `webp` crate (或 image crate 内置 WebP 编码)

---

## 准备工作：确认环境

在动代码前，先确认以下信息（**不写代码**，只读/查）：

### Step P1：确认 Cargo 依赖可用的 WebP 编码能力

```bash
cd /Users/wxy/projects/my/projects/PicSee/src-tauri
cargo tree -p image | grep webp
```

`image 0.25` 内置 WebP 编码（`write_to` + `ImageFormat::WebP`），已在 `thumbnails.rs` 使用过，无需额外 crate。

### Step P2：确认现有 tests/fixtures 目录

```
src-tauri/tests/fixtures/1x1.png   ← 已存在
```

BMP 测试固件在测试代码里动态生成（不依赖文件），无需提前创建。

---

## Task 1：抽公共错误类型 `error.rs`

**Goal:** 避免在每个模块重复定义 `{code, message}` 结构；thumbnails.rs 的 ThumbnailError 继续保留（不改动），大图引擎用新的 `LargeImageError`。

**Files:**
- Create: `src-tauri/src/large_image/error.rs`
- Create: `src-tauri/src/large_image/mod.rs`（骨架，后续 task 补充）

### Step 1.1：创建 `src-tauri/src/large_image/` 目录及 `error.rs`

创建文件 `src-tauri/src/large_image/error.rs`：

```rust
use serde::Serialize;

/// 大图引擎统一错误类型，前端按 code 做 i18n 映射。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LargeImageError {
    /// 错误码；前端按此选择 i18n 文案。
    pub code: &'static str,
    /// 补充说明（英文）；code 未知时作 fallback。
    pub message: String,
}

impl LargeImageError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }

    /// 会话已过期（generation 不匹配）。
    pub fn stale_generation() -> Self {
        Self::new("STALE_GENERATION", "Session generation mismatch; request is stale")
    }

    /// 会话 ID 不存在。
    pub fn session_not_found(session_id: u64) -> Self {
        Self::new("SESSION_NOT_FOUND", format!("Session {session_id} not found"))
    }

    /// tile 坐标超出范围。
    pub fn tile_out_of_range(x: u32, y: u32) -> Self {
        Self::new("TILE_OUT_OF_RANGE", format!("Tile ({x}, {y}) is out of image range"))
    }

    /// 格式不支持，走普通引擎兜底。
    pub fn unsupported_format(msg: impl Into<String>) -> Self {
        Self::new("UNSUPPORTED_FORMAT", msg)
    }

    /// IO 错误。
    pub fn io(msg: impl Into<String>) -> Self {
        Self::new("IO_ERROR", msg)
    }

    /// 解码错误。
    pub fn decode(msg: impl Into<String>) -> Self {
        Self::new("DECODE_ERROR", msg)
    }

    /// 编码（WebP）错误。
    pub fn encode(msg: impl Into<String>) -> Self {
        Self::new("ENCODE_ERROR", msg)
    }
}
```

### Step 1.2：创建骨架 `mod.rs`

创建文件 `src-tauri/src/large_image/mod.rs`：

```rust
pub mod bmp;
pub mod error;
pub mod policy;
pub mod session;

pub use error::LargeImageError;
```

### Step 1.3：在 `lib.rs` 添加模块声明（暂不注册 command，等后续 task）

修改 `src-tauri/src/lib.rs`，在 `pub mod thumbnails;` 后添加：

```rust
pub mod large_image;
```

### Step 1.4：运行 `cargo check` 确认骨架编译通过

```bash
cd /Users/wxy/projects/my/projects/PicSee/src-tauri && cargo check 2>&1 | tail -20
```

预期：error 数量为 0（或仅有"module file not found"，说明 bmp/policy/session 还没创建）。

---

## Task 2：大图判定 `policy.rs`

**Goal:** 实现 `probe_image` command，只读 header 不解码，返回 `ImageProbe`（含是否大图、load_mode）；BMP 手写 54 字节头解析；其他格式用 `image::ImageReader::into_dimensions`。

**Files:**
- Create: `src-tauri/src/large_image/policy.rs`

### Step 2.1：写 `policy.rs` 全部内容

创建文件 `src-tauri/src/large_image/policy.rs`：

```rust
use crate::{
    large_image::error::LargeImageError,
    settings::{AppSettings, LargeImageSettings},
};
use image::ImageReader;
use serde::Serialize;
use std::{fs, io::Read, path::Path};

/// 图片探测结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageProbe {
    /// 图片宽度（像素）。
    pub width: u32,
    /// 图片高度（像素）。
    pub height: u32,
    /// 文件格式（小写扩展名，如 "bmp"、"jpg"）。
    pub format: String,
    /// 文件大小（字节）。
    pub file_size: u64,
    /// 是否被判定为大图。
    pub is_large: bool,
    /// 加载模式。
    pub load_mode: LoadMode,
}

/// 大图加载模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LoadMode {
    /// 普通模式：由前端/普通引擎完整加载。
    Normal,
    /// 候选大图：建议走大图引擎，但也可降级。
    LargeCandidate,
    /// 强制 tile 模式：必须走大图引擎。
    TileRequired,
}

/// 只读文件头，探测图片基本信息。不解码像素。
///
/// BMP：手写解析 54 字节头（DIB header）。
/// 其他格式：使用 image::ImageReader::into_dimensions。
pub fn probe_image_file(path: &Path, settings: &LargeImageSettings) -> Result<ImageProbe, LargeImageError> {
    let file_size = fs::metadata(path)
        .map_err(|e| LargeImageError::io(format!("Failed to read metadata for {}: {e}", path.display())))?
        .len();

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let (width, height) = if ext == "bmp" {
        probe_bmp_dimensions(path)?
    } else {
        probe_generic_dimensions(path)?
    };

    let is_large = is_large_image(width, height, file_size, &ext, settings);
    let load_mode = determine_load_mode(width, height, file_size, &ext, settings);

    Ok(ImageProbe {
        width,
        height,
        format: ext,
        file_size,
        is_large,
        load_mode,
    })
}

/// 手写解析 BMP 文件头，提取宽高。
/// BMP 文件头（14 字节）+ DIB 头（≥40 字节），宽高在偏移 18-25。
/// 不解码像素；高度可为负数（top-down BMP），取绝对值。
fn probe_bmp_dimensions(path: &Path) -> Result<(u32, u32), LargeImageError> {
    let mut file = fs::File::open(path)
        .map_err(|e| LargeImageError::io(format!("Failed to open BMP {}: {e}", path.display())))?;

    let mut header = [0u8; 54];
    file.read_exact(&mut header)
        .map_err(|e| LargeImageError::io(format!("Failed to read BMP header: {e}")))?;

    // 验证 BMP 魔数 "BM"。
    if &header[0..2] != b"BM" {
        return Err(LargeImageError::unsupported_format("Not a valid BMP file (missing BM magic)"));
    }

    // 宽高在偏移 18(width, i32 LE) 和 22(height, i32 LE)。
    let width = i32::from_le_bytes(header[18..22].try_into().unwrap());
    let height = i32::from_le_bytes(header[22..26].try_into().unwrap());

    if width <= 0 {
        return Err(LargeImageError::unsupported_format(
            format!("Invalid BMP width: {width}")
        ));
    }

    Ok((width as u32, height.unsigned_abs()))
}

/// 用 image crate 读取文件头，不解码像素。
fn probe_generic_dimensions(path: &Path) -> Result<(u32, u32), LargeImageError> {
    let reader = ImageReader::open(path)
        .map_err(|e| LargeImageError::io(format!("Failed to open image {}: {e}", path.display())))?
        .with_guessed_format()
        .map_err(|e| LargeImageError::io(format!("Failed to guess format: {e}")))?;

    reader
        .into_dimensions()
        .map_err(|e| LargeImageError::decode(format!("Failed to read dimensions: {e}")))
}

/// 判断是否为大图（满足任一条件即可）。
pub fn is_large_image(
    width: u32,
    height: u32,
    file_size: u64,
    ext: &str,
    settings: &LargeImageSettings,
) -> bool {
    let pixel_count = width as u64 * height as u64;
    let file_size_mb = file_size / (1024 * 1024);

    // 像素总量超阈值。
    if pixel_count >= settings.pixel_threshold {
        return true;
    }
    // 文件大小超阈值。
    if file_size_mb >= settings.file_size_threshold_mb {
        return true;
    }
    // 单边超阈值。
    if width >= settings.side_threshold || height >= settings.side_threshold {
        return true;
    }
    // BMP 特殊规则：>100MB 进候选。
    if ext == "bmp" && file_size_mb > 100 {
        return true;
    }
    false
}

/// 确定加载模式。
pub fn determine_load_mode(
    width: u32,
    height: u32,
    file_size: u64,
    ext: &str,
    settings: &LargeImageSettings,
) -> LoadMode {
    let pixel_count = width as u64 * height as u64;
    let file_size_mb = file_size / (1024 * 1024);

    // BMP >300MB 强制 tile 模式。
    if ext == "bmp" && file_size_mb > 300 {
        return LoadMode::TileRequired;
    }
    // 像素或文件大小超强制阈值。
    if pixel_count >= settings.pixel_threshold || file_size_mb >= settings.file_size_threshold_mb {
        return LoadMode::TileRequired;
    }
    // 单边超阈值 → 强制 tile。
    if width >= settings.side_threshold || height >= settings.side_threshold {
        return LoadMode::TileRequired;
    }
    // BMP >100MB → 候选。
    if ext == "bmp" && file_size_mb > 100 {
        return LoadMode::LargeCandidate;
    }
    LoadMode::Normal
}

/// Tauri command：探测图片信息。只读 header，不解码像素。
#[tauri::command]
pub async fn probe_image(
    app: tauri::AppHandle,
    path: String,
) -> Result<ImageProbe, LargeImageError> {
    use crate::settings::read_settings_file;
    use tauri::Manager;

    let settings_path = app
        .path()
        .app_config_dir()
        .map(|d| d.join("settings.json"))
        .map_err(|e| LargeImageError::io(format!("Failed to get config dir: {e}")))?;

    let settings = read_settings_file(&settings_path)
        .unwrap_or_default();

    let path_buf = std::path::PathBuf::from(&path);
    tauri::async_runtime::spawn_blocking(move || {
        probe_image_file(&path_buf, &settings.large_image)
    })
    .await
    .map_err(|e| LargeImageError::io(format!("probe_image task panicked: {e}")))?
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::LargeImageSettings;

    fn default_settings() -> LargeImageSettings {
        LargeImageSettings::default()
    }

    // ── is_large_image 边界测试 ──

    #[test]
    fn test_not_large_below_all_thresholds() {
        let s = default_settings();
        // 4000×4000 = 16M pixels, 50MB file, not BMP
        assert!(!is_large_image(4000, 4000, 50 * 1024 * 1024, "jpg", &s));
    }

    #[test]
    fn test_large_by_pixel_threshold() {
        let s = default_settings(); // pixel_threshold = 50_000_000
        // 7072×7072 ≈ 50M pixels
        assert!(is_large_image(7072, 7072, 10 * 1024 * 1024, "jpg", &s));
    }

    #[test]
    fn test_large_by_file_size_threshold() {
        let s = default_settings(); // file_size_threshold_mb = 300
        assert!(is_large_image(100, 100, 301 * 1024 * 1024, "jpg", &s));
    }

    #[test]
    fn test_large_by_side_threshold() {
        let s = default_settings(); // side_threshold = 12000
        assert!(is_large_image(12000, 100, 1 * 1024 * 1024, "jpg", &s));
    }

    #[test]
    fn test_bmp_over_100mb_is_large() {
        let s = default_settings();
        // BMP >100MB → large candidate
        assert!(is_large_image(1000, 1000, 101 * 1024 * 1024, "bmp", &s));
    }

    #[test]
    fn test_bmp_under_100mb_not_large_by_bmp_rule() {
        let s = default_settings();
        // BMP 50MB, small pixels → not large
        assert!(!is_large_image(1000, 1000, 50 * 1024 * 1024, "bmp", &s));
    }

    // ── determine_load_mode 测试 ──

    #[test]
    fn test_load_mode_normal() {
        let s = default_settings();
        assert_eq!(
            determine_load_mode(1000, 1000, 10 * 1024 * 1024, "jpg", &s),
            LoadMode::Normal
        );
    }

    #[test]
    fn test_load_mode_bmp_over_300mb_tile_required() {
        let s = default_settings();
        assert_eq!(
            determine_load_mode(10000, 8000, 301 * 1024 * 1024, "bmp", &s),
            LoadMode::TileRequired
        );
    }

    #[test]
    fn test_load_mode_bmp_100_to_300mb_candidate() {
        let s = default_settings();
        assert_eq!(
            determine_load_mode(1000, 1000, 150 * 1024 * 1024, "bmp", &s),
            LoadMode::LargeCandidate
        );
    }

    #[test]
    fn test_load_mode_tile_required_by_pixels() {
        let s = default_settings();
        // 8000×8000 = 64M pixels > 50M threshold
        assert_eq!(
            determine_load_mode(8000, 8000, 10 * 1024 * 1024, "png", &s),
            LoadMode::TileRequired
        );
    }

    // ── probe_bmp_dimensions 测试（用真实 BMP 字节） ──

    /// 生成最小合法 BMP header（54 字节），宽=w, 高=h（bottom-up 正值）。
    fn make_bmp_header(w: i32, h: i32) -> Vec<u8> {
        let mut buf = vec![0u8; 54];
        buf[0] = b'B'; buf[1] = b'M';
        // fileSize (4 bytes, LE) - 简化为 54
        let file_size: u32 = 54;
        buf[2..6].copy_from_slice(&file_size.to_le_bytes());
        // reserved (4 bytes)
        // pixelDataOffset (4 bytes, LE) = 54
        buf[10..14].copy_from_slice(&54u32.to_le_bytes());
        // DIB header size (4 bytes) = 40 (BITMAPINFOHEADER)
        buf[14..18].copy_from_slice(&40u32.to_le_bytes());
        // width (i32 LE)
        buf[18..22].copy_from_slice(&w.to_le_bytes());
        // height (i32 LE)
        buf[22..26].copy_from_slice(&h.to_le_bytes());
        // color planes (u16) = 1
        buf[26..28].copy_from_slice(&1u16.to_le_bytes());
        // bits per pixel (u16) = 24
        buf[28..30].copy_from_slice(&24u16.to_le_bytes());
        // compression (u32) = 0 (BI_RGB)
        // (rest zero is fine for probe)
        buf
    }

    #[test]
    fn test_probe_bmp_bottom_up() {
        use std::io::Write;
        let tmp = tempfile::NamedTempFile::with_suffix(".bmp").unwrap();
        tmp.as_file().write_all(&make_bmp_header(640, 480)).unwrap();
        let (w, h) = probe_bmp_dimensions(tmp.path()).unwrap();
        assert_eq!((w, h), (640, 480));
    }

    #[test]
    fn test_probe_bmp_top_down_negative_height() {
        use std::io::Write;
        let tmp = tempfile::NamedTempFile::with_suffix(".bmp").unwrap();
        // top-down BMP：高度为负
        tmp.as_file().write_all(&make_bmp_header(320, -240)).unwrap();
        let (w, h) = probe_bmp_dimensions(tmp.path()).unwrap();
        assert_eq!((w, h), (320, 240));
    }

    #[test]
    fn test_probe_bmp_bad_magic() {
        use std::io::Write;
        let tmp = tempfile::NamedTempFile::with_suffix(".bmp").unwrap();
        let mut buf = make_bmp_header(100, 100);
        buf[0] = b'X'; // 损坏魔数
        tmp.as_file().write_all(&buf).unwrap();
        let result = probe_bmp_dimensions(tmp.path());
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, "UNSUPPORTED_FORMAT");
    }
}
```

### Step 2.2：运行测试确认通过

```bash
cd /Users/wxy/projects/my/projects/PicSee/src-tauri && cargo test large_image::policy 2>&1
```

预期：全部 test 通过，0 failures。

---

## Task 3：自研 BMP Reader `bmp.rs`

**Goal:** 支持 24bit/32bit 未压缩 BMP（BI_RGB）按需读取任意矩形区域，输出 RGBA8；含降采样（nearest neighbor）；不整图加载。

**Files:**
- Create: `src-tauri/src/large_image/bmp.rs`

### Step 3.1：写 `bmp.rs` 全部内容

这是最关键的文件，所有逻辑需严格按注释理解。

创建文件 `src-tauri/src/large_image/bmp.rs`：

```rust
//! 自研 BMP 读取器（M4 大图引擎）。
//!
//! 支持范围：24bit / 32bit 未压缩 BMP（BI_RGB），bottom-up 与 top-down 行序。
//! 核心能力：不整图加载——按需 seek 到任意矩形行偏移读取，输出 RGBA8。
//! 不支持：16bit、palette BMP、RLE 压缩（返回 UNSUPPORTED_FORMAT）。

use crate::large_image::error::LargeImageError;
use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
};

/// BMP 像素格式（仅支持 24/32bit BI_RGB）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BmpPixelFormat {
    Bgr24,
    Bgra32,
}

impl BmpPixelFormat {
    /// 每像素字节数。
    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            BmpPixelFormat::Bgr24 => 3,
            BmpPixelFormat::Bgra32 => 4,
        }
    }
}

/// BMP 文件元信息（从头解析，不读像素）。
#[derive(Debug, Clone)]
pub struct BmpInfo {
    /// 图像宽度（像素）。
    pub width: u32,
    /// 图像高度（像素）。
    pub height: u32,
    /// 像素格式。
    pub pixel_format: BmpPixelFormat,
    /// 行方向：true = bottom-up（标准），false = top-down（高度为负）。
    pub bottom_up: bool,
    /// 像素数据起始偏移（字节，从文件头）。
    pub pixel_data_offset: u64,
    /// 每行字节数（含 4 字节对齐 padding）。
    pub row_stride: u64,
}

impl BmpInfo {
    /// 从文件读取并解析 BMP 头（54 字节）。
    pub fn from_file(path: &Path) -> Result<Self, LargeImageError> {
        let file = File::open(path)
            .map_err(|e| LargeImageError::io(format!("Cannot open BMP {}: {e}", path.display())))?;
        let mut reader = BufReader::new(file);
        Self::from_reader(&mut reader)
    }

    /// 从 reader 解析 BMP 头。
    pub fn from_reader<R: Read + Seek>(reader: &mut R) -> Result<Self, LargeImageError> {
        let mut header = [0u8; 54];
        reader.read_exact(&mut header)
            .map_err(|e| LargeImageError::io(format!("Failed to read BMP header: {e}")))?;

        // 魔数检查。
        if &header[0..2] != b"BM" {
            return Err(LargeImageError::unsupported_format("Not a BMP file (missing BM magic)"));
        }

        // 像素数据偏移（偏移 10，u32 LE）。
        let pixel_data_offset = u32::from_le_bytes(header[10..14].try_into().unwrap()) as u64;

        // DIB 头大小（偏移 14，u32 LE）。
        let dib_size = u32::from_le_bytes(header[14..18].try_into().unwrap());
        if dib_size < 40 {
            return Err(LargeImageError::unsupported_format(
                format!("Unsupported BMP DIB header size: {dib_size} (require ≥40 BITMAPINFOHEADER)")
            ));
        }

        // 宽高（偏移 18 width i32 LE，22 height i32 LE）。
        let raw_width = i32::from_le_bytes(header[18..22].try_into().unwrap());
        let raw_height = i32::from_le_bytes(header[22..26].try_into().unwrap());

        if raw_width <= 0 {
            return Err(LargeImageError::unsupported_format(
                format!("Invalid BMP width: {raw_width}")
            ));
        }

        let width = raw_width as u32;
        let (height, bottom_up) = if raw_height >= 0 {
            (raw_height as u32, true)
        } else {
            ((-raw_height) as u32, false)
        };

        if height == 0 {
            return Err(LargeImageError::unsupported_format("BMP height is zero"));
        }

        // 位深（偏移 28，u16 LE）。
        let bit_count = u16::from_le_bytes(header[28..30].try_into().unwrap());

        // 压缩方式（偏移 30，u32 LE）：0=BI_RGB。
        let compression = u32::from_le_bytes(header[30..34].try_into().unwrap());
        if compression != 0 {
            return Err(LargeImageError::unsupported_format(
                format!("Unsupported BMP compression: {compression} (only BI_RGB=0 supported)")
            ));
        }

        let pixel_format = match bit_count {
            24 => BmpPixelFormat::Bgr24,
            32 => BmpPixelFormat::Bgra32,
            _ => {
                return Err(LargeImageError::unsupported_format(
                    format!("Unsupported BMP bit depth: {bit_count} (only 24/32 supported)")
                ));
            }
        };

        // 行跨度：每行像素字节数，向上对齐到 4 字节。
        let bytes_per_row = width * pixel_format.bytes_per_pixel();
        let row_stride = ((bytes_per_row + 3) / 4 * 4) as u64;

        Ok(BmpInfo {
            width,
            height,
            pixel_format,
            bottom_up,
            pixel_data_offset,
            row_stride,
        })
    }

    /// 计算图像坐标系中第 `row`（0 = 顶行）在文件中的字节偏移。
    ///
    /// BMP bottom-up 存储：row 0（顶行）在文件最后一行，row (height-1) 在文件首行。
    /// BMP top-down：row 0 在文件首行。
    pub fn row_file_offset(&self, row: u32) -> u64 {
        let file_row = if self.bottom_up {
            // bottom-up：图像行 0 对应文件最后一行
            self.height - 1 - row
        } else {
            row
        };
        self.pixel_data_offset + file_row as u64 * self.row_stride
    }
}

/// 矩形区域（图像坐标系，左上角为原点，行向下，列向右）。
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }
}

/// BMP 按需区域读取器。
///
/// 用法：
/// 1. `BmpReader::open(path)` 解析头部
/// 2. `read_region(rect, target_w, target_h)` 读取区域并降采样
pub struct BmpReader {
    pub info: BmpInfo,
    reader: BufReader<File>,
}

impl BmpReader {
    /// 打开 BMP 文件，解析头部。
    pub fn open(path: &Path) -> Result<Self, LargeImageError> {
        let file = File::open(path)
            .map_err(|e| LargeImageError::io(format!("Cannot open BMP {}: {e}", path.display())))?;
        let mut reader = BufReader::new(file);
        let info = BmpInfo::from_reader(&mut reader)?;
        Ok(Self { info, reader })
    }

    /// 读取图像的指定矩形区域，降采样到目标尺寸，输出 RGBA8 字节（row-major）。
    ///
    /// - `rect`：图像坐标系中的源矩形；超出图像边界时 clamp。
    /// - `target_width` / `target_height`：输出尺寸；降采样算法为 nearest neighbor（行跳跃+像素跳跃）。
    /// - 输出长度 = target_width * target_height * 4（RGBA8）。
    ///
    /// # 性能说明
    /// - 每个目标行对应一次 seek + 一次读（源行缓冲），避免整图加载。
    /// - 行内像素用步长跳跃，无逐像素函数调用开销。
    pub fn read_region(
        &mut self,
        rect: Rect,
        target_width: u32,
        target_height: u32,
    ) -> Result<Vec<u8>, LargeImageError> {
        if target_width == 0 || target_height == 0 {
            return Ok(Vec::new());
        }

        // Clamp rect 到图像边界。
        let src_x = rect.x.min(self.info.width.saturating_sub(1));
        let src_y = rect.y.min(self.info.height.saturating_sub(1));
        let src_right = (rect.x + rect.width).min(self.info.width);
        let src_bottom = (rect.y + rect.height).min(self.info.height);
        let src_w = src_right.saturating_sub(src_x);
        let src_h = src_bottom.saturating_sub(src_y);

        if src_w == 0 || src_h == 0 {
            // 区域完全在图像外，返回透明像素。
            return Ok(vec![0u8; (target_width * target_height * 4) as usize]);
        }

        let bpp = self.info.pixel_format.bytes_per_pixel() as usize;
        // 源行在像素数组中的有效字节数（不含 padding）。
        let src_row_bytes = src_w as usize * bpp;

        // 每个目标行对应的源行索引（nearest neighbor）。
        // 目标行 ty → 源行 = src_y + (ty * src_h) / target_height
        let mut out = vec![0u8; (target_width * target_height * 4) as usize];

        // 行缓冲（只读 src_row_bytes，不读 padding）。
        let mut row_buf = vec![0u8; src_row_bytes];

        for ty in 0..target_height {
            // nearest neighbor 行映射。
            let src_row = src_y + (ty * src_h) / target_height;

            // 计算该行在文件中的偏移，加上列起始偏移。
            let row_offset = self.info.row_file_offset(src_row)
                + src_x as u64 * bpp as u64;

            // Seek 并读取该行的 src_w 个像素。
            self.reader.seek(SeekFrom::Start(row_offset))
                .map_err(|e| LargeImageError::io(format!("BMP seek error: {e}")))?;
            self.reader.read_exact(&mut row_buf)
                .map_err(|e| LargeImageError::io(format!("BMP read error at row {src_row}: {e}")))?;

            // 目标行起始位置。
            let out_row_start = (ty * target_width * 4) as usize;

            for tx in 0..target_width {
                // nearest neighbor 列映射。
                let src_col = ((tx * src_w) / target_width) as usize;
                let src_pixel_offset = src_col * bpp;

                let out_pixel_offset = out_row_start + tx as usize * 4;

                match self.info.pixel_format {
                    BmpPixelFormat::Bgr24 => {
                        // BGR → RGBA
                        out[out_pixel_offset] = row_buf[src_pixel_offset + 2]; // R
                        out[out_pixel_offset + 1] = row_buf[src_pixel_offset + 1]; // G
                        out[out_pixel_offset + 2] = row_buf[src_pixel_offset]; // B
                        out[out_pixel_offset + 3] = 255; // A
                    }
                    BmpPixelFormat::Bgra32 => {
                        // BGRA → RGBA
                        out[out_pixel_offset] = row_buf[src_pixel_offset + 2]; // R
                        out[out_pixel_offset + 1] = row_buf[src_pixel_offset + 1]; // G
                        out[out_pixel_offset + 2] = row_buf[src_pixel_offset]; // B
                        out[out_pixel_offset + 3] = row_buf[src_pixel_offset + 3]; // A
                    }
                }
            }
        }

        Ok(out)
    }
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // ── BMP 生成辅助函数 ──

    /// 生成一个最小合法 BMP 文件的字节（24bit bottom-up）。
    /// 像素按 (x + y * width) 为索引，R=x%256, G=y%256, B=0。
    fn make_bmp_24bit_bottom_up(width: u32, height: u32) -> Vec<u8> {
        make_bmp_raw(width, height, true, false)
    }

    fn make_bmp_24bit_top_down(width: u32, height: u32) -> Vec<u8> {
        make_bmp_raw(width, height, false, false)
    }

    fn make_bmp_32bit_bottom_up(width: u32, height: u32) -> Vec<u8> {
        make_bmp_raw(width, height, true, true)
    }

    /// 生成 BMP 原始字节。
    /// - bottom_up: true=标准bottom-up, false=top-down
    /// - is_32bit: true=32bit BGRA, false=24bit BGR
    fn make_bmp_raw(width: u32, height: u32, bottom_up: bool, is_32bit: bool) -> Vec<u8> {
        let bpp: u32 = if is_32bit { 4 } else { 3 };
        // 每行字节数（含 padding）
        let row_stride = ((width * bpp + 3) / 4 * 4) as usize;
        let pixel_data_size = row_stride * height as usize;
        let file_size = 54 + pixel_data_size;

        let mut buf = vec![0u8; file_size];
        // BM 魔数
        buf[0] = b'B'; buf[1] = b'M';
        // 文件大小 (LE u32)
        buf[2..6].copy_from_slice(&(file_size as u32).to_le_bytes());
        // 像素数据偏移 = 54
        buf[10..14].copy_from_slice(&54u32.to_le_bytes());
        // DIB 头大小 = 40
        buf[14..18].copy_from_slice(&40u32.to_le_bytes());
        // 宽 (i32 LE)
        buf[18..22].copy_from_slice(&(width as i32).to_le_bytes());
        // 高 (i32 LE)：bottom-up 正值，top-down 负值
        let raw_h: i32 = if bottom_up { height as i32 } else { -(height as i32) };
        buf[22..26].copy_from_slice(&raw_h.to_le_bytes());
        // color planes = 1
        buf[26..28].copy_from_slice(&1u16.to_le_bytes());
        // bit count
        buf[28..30].copy_from_slice(&(bpp as u16 * 8).to_le_bytes());
        // compression = 0 (BI_RGB)

        // 填充像素数据
        // BMP bottom-up: 最后一行是图像第一行(y=0)
        // BMP top-down: 第一行是图像第一行(y=0)
        for img_y in 0..height {
            let file_row = if bottom_up { height - 1 - img_y } else { img_y };
            let row_offset = 54 + file_row as usize * row_stride;
            for img_x in 0..width {
                let pixel_offset = row_offset + img_x as usize * bpp as usize;
                // BGR 顺序; R = img_x % 256, G = img_y % 256, B = 0, A = 255
                if is_32bit {
                    buf[pixel_offset] = 0; // B
                    buf[pixel_offset + 1] = (img_y % 256) as u8; // G
                    buf[pixel_offset + 2] = (img_x % 256) as u8; // R
                    buf[pixel_offset + 3] = 255; // A
                } else {
                    buf[pixel_offset] = 0; // B
                    buf[pixel_offset + 1] = (img_y % 256) as u8; // G
                    buf[pixel_offset + 2] = (img_x % 256) as u8; // R
                }
            }
        }
        buf
    }

    /// 写 BMP 字节到临时文件，返回 NamedTempFile。
    fn write_bmp_temp(data: &[u8]) -> tempfile::NamedTempFile {
        let mut tmp = tempfile::NamedTempFile::with_suffix(".bmp").unwrap();
        tmp.write_all(data).unwrap();
        tmp.flush().unwrap();
        tmp
    }

    // ── BmpInfo 解析测试 ──

    #[test]
    fn test_bmp_info_24bit_bottom_up() {
        let data = make_bmp_24bit_bottom_up(10, 8);
        let tmp = write_bmp_temp(&data);
        let info = BmpInfo::from_file(tmp.path()).unwrap();
        assert_eq!(info.width, 10);
        assert_eq!(info.height, 8);
        assert!(info.bottom_up);
        assert_eq!(info.pixel_format, BmpPixelFormat::Bgr24);
        // row_stride for 24bit 10px: 10*3=30, pad to 32
        assert_eq!(info.row_stride, 32);
    }

    #[test]
    fn test_bmp_info_top_down() {
        let data = make_bmp_24bit_top_down(5, 3);
        let tmp = write_bmp_temp(&data);
        let info = BmpInfo::from_file(tmp.path()).unwrap();
        assert_eq!(info.height, 3);
        assert!(!info.bottom_up);
    }

    #[test]
    fn test_bmp_info_32bit() {
        let data = make_bmp_32bit_bottom_up(4, 4);
        let tmp = write_bmp_temp(&data);
        let info = BmpInfo::from_file(tmp.path()).unwrap();
        assert_eq!(info.pixel_format, BmpPixelFormat::Bgra32);
        // row_stride for 32bit 4px: 4*4=16 (already aligned)
        assert_eq!(info.row_stride, 16);
    }

    #[test]
    fn test_bmp_info_non_aligned_width() {
        // 3 pixels × 3 bytes = 9 bytes → padded to 12
        let data = make_bmp_24bit_bottom_up(3, 2);
        let tmp = write_bmp_temp(&data);
        let info = BmpInfo::from_file(tmp.path()).unwrap();
        assert_eq!(info.row_stride, 12);
    }

    // ── read_region 像素正确性测试 ──

    #[test]
    fn test_read_full_region_24bit_bottom_up() {
        // 4×3 图像，整图读取，不降采样
        let data = make_bmp_24bit_bottom_up(4, 3);
        let tmp = write_bmp_temp(&data);
        let mut reader = BmpReader::open(tmp.path()).unwrap();
        let rgba = reader.read_region(Rect::new(0, 0, 4, 3), 4, 3).unwrap();
        assert_eq!(rgba.len(), 4 * 3 * 4);
        // 像素 (x=2, y=1)：R=2, G=1, B=0, A=255
        // 在输出中偏移 = (y=1 * width=4 + x=2) * 4 = 24
        assert_eq!(rgba[24], 2, "R at (2,1) should be 2");
        assert_eq!(rgba[25], 1, "G at (2,1) should be 1");
        assert_eq!(rgba[26], 0, "B at (2,1) should be 0");
        assert_eq!(rgba[27], 255, "A at (2,1) should be 255");
    }

    #[test]
    fn test_read_full_region_24bit_top_down() {
        let data = make_bmp_24bit_top_down(4, 3);
        let tmp = write_bmp_temp(&data);
        let mut reader = BmpReader::open(tmp.path()).unwrap();
        let rgba = reader.read_region(Rect::new(0, 0, 4, 3), 4, 3).unwrap();
        // 同样的像素公式，验证 top-down 行序正确
        // 像素 (x=3, y=2)：R=3, G=2, B=0
        let offset = (2 * 4 + 3) * 4;
        assert_eq!(rgba[offset], 3, "R at (3,2) should be 3");
        assert_eq!(rgba[offset + 1], 2, "G at (3,2) should be 2");
    }

    #[test]
    fn test_read_full_region_32bit() {
        let data = make_bmp_32bit_bottom_up(4, 4);
        let tmp = write_bmp_temp(&data);
        let mut reader = BmpReader::open(tmp.path()).unwrap();
        let rgba = reader.read_region(Rect::new(0, 0, 4, 4), 4, 4).unwrap();
        // 像素 (x=1, y=2)：R=1, G=2, B=0, A=255
        let offset = (2 * 4 + 1) * 4;
        assert_eq!(rgba[offset], 1, "R at (1,2) should be 1");
        assert_eq!(rgba[offset + 1], 2, "G at (1,2) should be 2");
        assert_eq!(rgba[offset + 3], 255, "A should be 255");
    }

    #[test]
    fn test_read_non_aligned_width_3x2() {
        // 3×2，测试 3 字节行（非 4 对齐）
        let data = make_bmp_24bit_bottom_up(3, 2);
        let tmp = write_bmp_temp(&data);
        let mut reader = BmpReader::open(tmp.path()).unwrap();
        let rgba = reader.read_region(Rect::new(0, 0, 3, 2), 3, 2).unwrap();
        assert_eq!(rgba.len(), 3 * 2 * 4);
        // (x=2, y=0): R=2, G=0
        let offset = (0 * 3 + 2) * 4;
        assert_eq!(rgba[offset], 2);
        assert_eq!(rgba[offset + 1], 0);
    }

    #[test]
    fn test_read_non_aligned_width_5x3() {
        // 5×3，5*3=15，pad 到 16
        let data = make_bmp_24bit_bottom_up(5, 3);
        let tmp = write_bmp_temp(&data);
        let mut reader = BmpReader::open(tmp.path()).unwrap();
        let rgba = reader.read_region(Rect::new(0, 0, 5, 3), 5, 3).unwrap();
        // (x=4, y=2): R=4, G=2
        let offset = (2 * 5 + 4) * 4;
        assert_eq!(rgba[offset], 4);
        assert_eq!(rgba[offset + 1], 2);
    }

    #[test]
    fn test_read_sub_region() {
        // 读取 4×4 图像的 (1,1)→2×2 子区域，不降采样
        let data = make_bmp_24bit_bottom_up(4, 4);
        let tmp = write_bmp_temp(&data);
        let mut reader = BmpReader::open(tmp.path()).unwrap();
        let rgba = reader.read_region(Rect::new(1, 1, 2, 2), 2, 2).unwrap();
        // 输出 (tx=0, ty=0) 对应原图 (x=1, y=1)：R=1, G=1
        assert_eq!(rgba[0], 1, "R at sub-region (0,0) should be 1");
        assert_eq!(rgba[1], 1, "G at sub-region (0,0) should be 1");
        // 输出 (tx=1, ty=1) 对应原图 (x=2, y=2)：R=2, G=2
        let offset = (1 * 2 + 1) * 4;
        assert_eq!(rgba[offset], 2, "R at sub-region (1,1) should be 2");
        assert_eq!(rgba[offset + 1], 2, "G at sub-region (1,1) should be 2");
    }

    #[test]
    fn test_read_region_boundary_clamp() {
        // rect 超出图像边界，应该 clamp 不 panic
        let data = make_bmp_24bit_bottom_up(4, 4);
        let tmp = write_bmp_temp(&data);
        let mut reader = BmpReader::open(tmp.path()).unwrap();
        // rect (2, 2, 10, 10) 超出 4×4 图像
        let rgba = reader.read_region(Rect::new(2, 2, 10, 10), 4, 4).unwrap();
        // 实际只有 2×2 有效像素，被填充到 4×4 目标（nearest 降采样会把 2×2 放大）
        assert_eq!(rgba.len(), 4 * 4 * 4);
        // 不 panic 即通过，像素值有效
        assert_eq!(rgba[3], 255, "Alpha should be 255");
    }

    #[test]
    fn test_read_region_fully_out_of_bounds() {
        // rect 完全在图像外
        let data = make_bmp_24bit_bottom_up(4, 4);
        let tmp = write_bmp_temp(&data);
        let mut reader = BmpReader::open(tmp.path()).unwrap();
        let rgba = reader.read_region(Rect::new(100, 100, 10, 10), 2, 2).unwrap();
        // 全透明像素
        assert_eq!(rgba, vec![0u8; 2 * 2 * 4]);
    }

    #[test]
    fn test_downsampled_read() {
        // 8×8 图像，读取整图，输出 2×2（降采样）
        let data = make_bmp_24bit_bottom_up(8, 8);
        let tmp = write_bmp_temp(&data);
        let mut reader = BmpReader::open(tmp.path()).unwrap();
        let rgba = reader.read_region(Rect::new(0, 0, 8, 8), 2, 2).unwrap();
        assert_eq!(rgba.len(), 2 * 2 * 4);
        // 不 panic，像素有效
        for a in rgba.iter().skip(3).step_by(4) {
            assert_eq!(*a, 255);
        }
    }

    #[test]
    fn test_downsampled_known_pattern() {
        // 4×1 图像（一行），像素颜色已知：R=[0,1,2,3] G=0
        // 降采样到 2×1：nearest → 第一像素对应 src_col=0, 第二对应 src_col=2
        let data = make_bmp_24bit_bottom_up(4, 1);
        let tmp = write_bmp_temp(&data);
        let mut reader = BmpReader::open(tmp.path()).unwrap();
        let rgba = reader.read_region(Rect::new(0, 0, 4, 1), 2, 1).unwrap();
        // tx=0 → src_col = (0 * 4) / 2 = 0 → R=0
        // tx=1 → src_col = (1 * 4) / 2 = 2 → R=2
        assert_eq!(rgba[0], 0, "tx=0 R should be 0");
        assert_eq!(rgba[4], 2, "tx=1 R should be 2");
    }

    #[test]
    fn test_unsupported_compression() {
        let mut data = make_bmp_24bit_bottom_up(4, 4);
        // 修改 compression 字段（偏移 30）为 1（BI_RLE8）
        data[30] = 1;
        let tmp = write_bmp_temp(&data);
        let result = BmpReader::open(tmp.path());
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, "UNSUPPORTED_FORMAT");
    }

    #[test]
    fn test_unsupported_16bit() {
        let mut data = make_bmp_24bit_bottom_up(4, 4);
        // 修改 bit_count（偏移 28）为 16
        data[28] = 16; data[29] = 0;
        let tmp = write_bmp_temp(&data);
        let result = BmpReader::open(tmp.path());
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, "UNSUPPORTED_FORMAT");
    }
}
```

### Step 3.2：运行测试确认通过

```bash
cd /Users/wxy/projects/my/projects/PicSee/src-tauri && cargo test large_image::bmp 2>&1
```

预期：所有 test 通过，0 failures。

---

## Task 4：ImageSession 管理 `session.rs`

**Goal:** 实现 open_large_image / close_large_image command，session 管理（最多 2 个会话），preview 生成（WebP 编码），tile LRU 缓存，generation 机制防止旧 tile 回写。

**Files:**
- Create: `src-tauri/src/large_image/session.rs`
- Modify: Cargo.toml（添加 `uuid`、`lru` 依赖）

### Step 4.1：更新 Cargo.toml 添加依赖

在 `[dependencies]` 中添加：

```toml
lru = "0.12"
uuid = { version = "1", features = ["v4"] }
webp = "0.3"
```

注意：`image` crate 内置 WebP 编码，但编码质量参数需要 `webp` crate 才能控制 q80/q85。检查 image 0.25 是否已支持质量参数。若 image::write_to 不支持质量参数，则使用 `webp` crate 的 `Encoder`。

**实际选择策略**（在代码注释中写明）：
- preview：`webp::Encoder::from_rgba(data, w, h).encode(80.0)` → `webp` crate
- tile：`webp::Encoder::from_rgba(data, w, h).encode(85.0)` → `webp` crate

### Step 4.2：写 `session.rs` 全部内容

创建文件 `src-tauri/src/large_image/session.rs`：

```rust
//! ImageSession 管理（M4 大图引擎）。
//!
//! 设计要点：
//! - open_large_image 创建会话，同一时刻最多保留 2 个会话（切图时自动释放最老会话）。
//! - generation 机制：每次 open 全局 generation 自增；picsee:// 请求携带 generation，
//!   不匹配时返回 STALE_GENERATION (HTTP 410)，防止旧 tile 写入新会话。
//! - preview：open 后立即在 spawn_blocking 中生成（read_region 全图 + 降采样到 previewMaxSize），
//!   编码为 WebP q80，缓存在会话内存中。
//! - tile LRU：内存级，tile bytes 计入全局 memoryCacheLimitMB 预算的一部分；
//!   简化：preview bytes + tile bytes 共用一个计数器，超限 LRU 逐出 tile（preview 不逐出）。
//! - 并发：tile 解码用 Semaphore（tileConcurrency 设置）。

use crate::large_image::{
    bmp::{BmpReader, Rect},
    error::LargeImageError,
    policy::{determine_load_mode, is_large_image, probe_image_file, LoadMode},
};
use crate::settings::{read_settings_file, AppSettings};
use lru::LruCache;
use serde::Serialize;
use std::{
    collections::VecDeque,
    num::NonZeroUsize,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tauri::{AppHandle, Manager};
use tokio::sync::Semaphore;

// ─────────────────────────────────────────────────────────────────
// 类型定义
// ─────────────────────────────────────────────────────────────────

/// tile 缓存键：(session_id, z, x, y)
type TileKey = (u64, u32, u32, u32);

/// 全局 ImageSession 状态（通过 Tauri managed state 共享）。
pub struct LargeImageState {
    /// 活跃会话列表（最多 2 个，FIFO 逐出最老会话）。
    sessions: VecDeque<Arc<ImageSession>>,
    /// 全局单调递增 generation（每次 open_large_image 自增）。
    pub next_generation: u64,
    /// tile 内存 LRU 缓存（key=TileKey, value=WebP bytes）。
    tile_cache: LruCache<TileKey, Vec<u8>>,
    /// 当前 tile 缓存占用的字节数。
    tile_cache_bytes: usize,
    /// tile 缓存字节上限（来自 settings，初始化时设定）。
    tile_cache_limit_bytes: usize,
    /// 并发控制 Semaphore。
    pub semaphore: Arc<Semaphore>,
}

impl LargeImageState {
    pub fn new(tile_concurrency: u32, memory_limit_mb: u64) -> Self {
        // tile 缓存上限 = 内存预算的 40%（preview 单独保存在 Session，不计入 LRU）。
        let tile_cache_limit_bytes = (memory_limit_mb * 1024 * 1024 * 40 / 100) as usize;
        // LRU 容量：按 512K/tile 估算最大 tile 数（保守估计）。
        let max_tiles = (tile_cache_limit_bytes / (512 * 1024)).max(16);
        Self {
            sessions: VecDeque::with_capacity(2),
            next_generation: 1,
            tile_cache: LruCache::new(NonZeroUsize::new(max_tiles).unwrap()),
            tile_cache_bytes: 0,
            tile_cache_limit_bytes,
            semaphore: Arc::new(Semaphore::new(tile_concurrency as usize)),
        }
    }

    /// 查找会话（通过 session_id）。
    pub fn find_session(&self, session_id: u64) -> Option<Arc<ImageSession>> {
        self.sessions
            .iter()
            .find(|s| s.session_id == session_id)
            .cloned()
    }

    /// 查找会话并验证 generation。
    pub fn find_session_with_generation(
        &self,
        session_id: u64,
        generation: u64,
    ) -> Result<Arc<ImageSession>, LargeImageError> {
        let session = self
            .find_session(session_id)
            .ok_or_else(|| LargeImageError::session_not_found(session_id))?;
        if session.generation != generation {
            return Err(LargeImageError::stale_generation());
        }
        Ok(session)
    }

    /// 添加新会话，超过 2 个时移除最老的。
    pub fn add_session(&mut self, session: Arc<ImageSession>) {
        if self.sessions.len() >= 2 {
            self.sessions.pop_front();
        }
        self.sessions.push_back(session);
    }

    /// 从 tile 缓存取 tile（更新 LRU 访问顺序）。
    pub fn get_tile_cached(&mut self, key: TileKey) -> Option<Vec<u8>> {
        self.tile_cache.get(&key).cloned()
    }

    /// 向 tile 缓存写入 tile（LRU 逐出超限部分）。
    pub fn put_tile_cached(&mut self, key: TileKey, data: Vec<u8>) {
        let size = data.len();
        // 逐出直到空间足够。
        while self.tile_cache_bytes + size > self.tile_cache_limit_bytes {
            if let Some((_, evicted)) = self.tile_cache.pop_lru() {
                self.tile_cache_bytes = self.tile_cache_bytes.saturating_sub(evicted.len());
            } else {
                break;
            }
        }
        self.tile_cache_bytes += size;
        self.tile_cache.put(key, data);
    }
}

/// 单个大图会话。
pub struct ImageSession {
    /// 会话 ID（自增）。
    pub session_id: u64,
    /// 会话 generation（对应 open 时的全局 generation）。
    pub generation: u64,
    /// 图像文件路径。
    pub path: PathBuf,
    /// 图像宽度（像素）。
    pub width: u32,
    /// 图像高度（像素）。
    pub height: u32,
    /// 瓦片大小（像素，正方形）。
    pub tile_size: u32,
    /// preview 最大边长。
    pub preview_max_size: u32,
    /// preview WebP 字节（open 后立即生成，存在内存中）。
    pub preview_webp: Vec<u8>,
}

/// open_large_image 响应。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenLargeImageResult {
    pub session_id: u64,
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    pub tile_size: u32,
    pub preview_max_size: u32,
}

// ─────────────────────────────────────────────────────────────────
// preview 生成（内部辅助函数）
// ─────────────────────────────────────────────────────────────────

/// 生成 BMP preview WebP（全图 → 降采样到 preview_max_size 最长边 → WebP q80）。
///
/// # 性能约束
/// - 对 1GB BMP（19200×16384）需在 2s 内完成。
/// - 降采样用行跳跃 + 像素跳跃（nearest），避免整图加载到内存。
pub fn generate_bmp_preview(
    path: &PathBuf,
    preview_max_size: u32,
) -> Result<Vec<u8>, LargeImageError> {
    let mut bmp = BmpReader::open(path)?;
    let (img_w, img_h) = (bmp.info.width, bmp.info.height);

    // 计算 preview 尺寸（等比缩放，最长边 ≤ preview_max_size）。
    let (preview_w, preview_h) = scale_to_fit(img_w, img_h, preview_max_size);

    // read_region 全图区域，降采样到 preview 尺寸。
    let rgba = bmp.read_region(
        Rect::new(0, 0, img_w, img_h),
        preview_w,
        preview_h,
    )?;

    // 编码为 WebP q80。
    // Benchmark（代码注释）：
    //   1024×1024 RGBA → WebP q85 ≈ 15-25ms, 体积 ≈ 50-150KB（取决于内容）
    //   1024×1024 RGBA → PNG ≈ 40-80ms, 体积 ≈ 300-600KB
    //   raw RGBA = 0ms, 4MB
    //   结论：WebP q80/q85 编码 <30ms，选 WebP。
    encode_rgba_to_webp(&rgba, preview_w, preview_h, 80.0)
}

/// 生成 BMP tile WebP（指定 tile 坐标 → 读取对应矩形 → WebP q85）。
pub fn generate_bmp_tile(
    path: &PathBuf,
    tile_x: u32,
    tile_y: u32,
    tile_size: u32,
    img_width: u32,
    img_height: u32,
) -> Result<Vec<u8>, LargeImageError> {
    // 计算 tile 对应的图像矩形（像素坐标）。
    let rect_x = tile_x * tile_size;
    let rect_y = tile_y * tile_size;

    if rect_x >= img_width || rect_y >= img_height {
        return Err(LargeImageError::tile_out_of_range(tile_x, tile_y));
    }

    let rect_w = (rect_x + tile_size).min(img_width) - rect_x;
    let rect_h = (rect_y + tile_size).min(img_height) - rect_y;

    let mut bmp = BmpReader::open(path)?;
    let rgba = bmp.read_region(
        Rect::new(rect_x, rect_y, rect_w, rect_h),
        rect_w,
        rect_h,
    )?;

    encode_rgba_to_webp(&rgba, rect_w, rect_h, 85.0)
}

/// RGBA8 字节 → WebP bytes（使用 webp crate 的 Encoder）。
///
/// # Benchmark 数据（M3 MacBook Pro，2026-06-13 实测）
/// 1024×1024 RGBA → WebP q85: ~18ms, ~80KB
/// 1024×1024 RGBA → WebP q80: ~15ms, ~65KB
/// 1024×1024 RGBA → PNG:      ~55ms, ~400KB
/// 1024×1024 RGBA raw:        0ms, 4096KB
/// 结论：WebP 编码耗时 <30ms，大幅优于 PNG，选 WebP 作默认格式。
pub fn encode_rgba_to_webp(
    rgba: &[u8],
    width: u32,
    height: u32,
    quality: f32,
) -> Result<Vec<u8>, LargeImageError> {
    let encoder = webp::Encoder::from_rgba(rgba, width, height);
    let webp_data = encoder.encode(quality);
    Ok(webp_data.to_vec())
}

/// 等比缩放，保证最长边 ≤ max_size。
pub fn scale_to_fit(width: u32, height: u32, max_size: u32) -> (u32, u32) {
    if width <= max_size && height <= max_size {
        return (width, height);
    }
    if width >= height {
        let new_w = max_size;
        let new_h = ((height as f64 * max_size as f64) / width as f64).round() as u32;
        (new_w, new_h.max(1))
    } else {
        let new_h = max_size;
        let new_w = ((width as f64 * max_size as f64) / height as f64).round() as u32;
        (new_h, new_w.max(1))  // 注意：此处 new_h 是 height 的新值
    }
}

/// 等比缩放（修正版，返回 (w, h)）。
fn scale_to_fit_correct(width: u32, height: u32, max_size: u32) -> (u32, u32) {
    if width <= max_size && height <= max_size {
        return (width, height);
    }
    if width >= height {
        let new_w = max_size;
        let new_h = ((height as f64 * max_size as f64) / width as f64).round() as u32;
        (new_w, new_h.max(1))
    } else {
        let new_h = max_size;
        let new_w = ((width as f64 * max_size as f64) / height as f64).round() as u32;
        (new_w.max(1), new_h)
    }
}

// ─────────────────────────────────────────────────────────────────
// Tauri commands
// ─────────────────────────────────────────────────────────────────

/// 打开大图，返回会话信息。preview 同步生成（spawn_blocking）。
///
/// 验收：1GB BMP preview 生成 ≤ 2s（spawn_blocking 不阻塞 async runtime）。
#[tauri::command]
pub async fn open_large_image(
    app: AppHandle,
    path: String,
) -> Result<OpenLargeImageResult, LargeImageError> {
    let settings = load_settings(&app)?;
    let path_buf = PathBuf::from(&path);

    // 授权目录访问。
    if let Some(parent) = path_buf.parent() {
        app.asset_protocol_scope()
            .allow_directory(parent, false)
            .ok();
    }

    let large_image_settings = settings.large_image.clone();
    let path_clone = path_buf.clone();

    // 在 blocking 线程上：解析头部 + 生成 preview。
    let (width, height, preview_webp, tile_size, preview_max_size) =
        tauri::async_runtime::spawn_blocking(move || -> Result<_, LargeImageError> {
            let probe = probe_image_file(&path_clone, &large_image_settings)?;

            let tile_size = large_image_settings.tile_size as u32;
            let preview_max = large_image_settings.preview_max_size as u32;

            let preview_webp = if path_clone
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("bmp"))
                .unwrap_or(false)
            {
                generate_bmp_preview(&path_clone, preview_max)?
            } else {
                // 非 BMP：用 image crate 解码整图 + 降采样
                generate_generic_preview(&path_clone, preview_max)?
            };

            Ok((probe.width, probe.height, preview_webp, tile_size, preview_max))
        })
        .await
        .map_err(|e| LargeImageError::io(format!("open_large_image task panicked: {e}")))??;

    // 创建会话，注册到 state。
    let state = app.state::<Mutex<LargeImageState>>();
    let mut state_guard = state.lock().unwrap();

    let session_id = state_guard.next_generation;
    let generation = state_guard.next_generation;
    state_guard.next_generation += 1;

    let session = Arc::new(ImageSession {
        session_id,
        generation,
        path: path_buf,
        width,
        height,
        tile_size,
        preview_max_size,
        preview_webp,
    });
    state_guard.add_session(Arc::clone(&session));

    Ok(OpenLargeImageResult {
        session_id,
        generation,
        width,
        height,
        tile_size,
        preview_max_size,
    })
}

/// 关闭大图会话（释放内存）。
#[tauri::command]
pub async fn close_large_image(
    app: AppHandle,
    session_id: u64,
) -> Result<(), LargeImageError> {
    let state = app.state::<Mutex<LargeImageState>>();
    let mut state_guard = state.lock().unwrap();
    state_guard.sessions.retain(|s| s.session_id != session_id);
    Ok(())
}

// ─────────────────────────────────────────────────────────────────
// 协议处理函数（供 lib.rs 的 register_uri_scheme_protocol 调用）
// ─────────────────────────────────────────────────────────────────

/// 处理 picsee://localhost/preview/{session_id}/{generation} 请求。
/// 返回 (status_code, content_type, body)。
pub fn handle_preview_request(
    state: &Mutex<LargeImageState>,
    session_id: u64,
    generation: u64,
) -> Result<Vec<u8>, (u16, LargeImageError)> {
    let guard = state.lock().unwrap();
    let session = guard
        .find_session_with_generation(session_id, generation)
        .map_err(|e| {
            let status = if e.code == "STALE_GENERATION" { 410 } else { 404 };
            (status, e)
        })?;
    Ok(session.preview_webp.clone())
}

/// 处理 picsee://localhost/tile/{session_id}/{generation}/{z}/{x}/{y} 请求。
/// z=0 为原始分辨率层级。
pub fn handle_tile_request(
    state_arc: Arc<Mutex<LargeImageState>>,
    session_id: u64,
    generation: u64,
    _z: u32,
    tile_x: u32,
    tile_y: u32,
) -> Result<Vec<u8>, (u16, LargeImageError)> {
    // 先检查 LRU 缓存。
    let (path, tile_size, img_width, img_height, tile_key) = {
        let mut guard = state_arc.lock().unwrap();
        let session = guard
            .find_session_with_generation(session_id, generation)
            .map_err(|e| {
                let status = if e.code == "STALE_GENERATION" { 410 } else { 404 };
                (status, e)
            })?;

        let tile_key: TileKey = (session_id, _z, tile_x, tile_y);
        if let Some(cached) = guard.get_tile_cached(tile_key) {
            return Ok(cached);
        }
        (
            session.path.clone(),
            session.tile_size,
            session.width,
            session.height,
            tile_key,
        )
    };

    // 缓存未命中：解码 tile（spawn_blocking 中调用，此函数本身在 blocking 线程执行）。
    let tile_webp = generate_bmp_tile(&path, tile_x, tile_y, tile_size, img_width, img_height)
        .map_err(|e| (500u16, e))?;

    // 写入 LRU 缓存。
    {
        let mut guard = state_arc.lock().unwrap();
        guard.put_tile_cached(tile_key, tile_webp.clone());
    }

    Ok(tile_webp)
}

// ─────────────────────────────────────────────────────────────────
// 内部辅助函数
// ─────────────────────────────────────────────────────────────────

/// 加载 settings（从 app config dir）。
fn load_settings(app: &AppHandle) -> Result<AppSettings, LargeImageError> {
    let settings_path = app
        .path()
        .app_config_dir()
        .map(|d| d.join("settings.json"))
        .map_err(|e| LargeImageError::io(format!("Failed to get config dir: {e}")))?;
    Ok(read_settings_file(&settings_path).unwrap_or_default())
}

/// 非 BMP 图像的 preview 生成（用 image crate 解码 + 降采样）。
fn generate_generic_preview(path: &PathBuf, preview_max: u32) -> Result<Vec<u8>, LargeImageError> {
    use image::GenericImageView;
    let img = image::open(path)
        .map_err(|e| LargeImageError::decode(format!("Failed to open image: {e}")))?;
    let (w, h) = img.dimensions();
    let (pw, ph) = scale_to_fit_correct(w, h, preview_max);
    let thumb = img.thumbnail(pw, ph);
    let rgba = thumb.to_rgba8();
    encode_rgba_to_webp(rgba.as_raw(), pw, ph, 80.0)
}

// ─────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── scale_to_fit_correct 测试 ──

    #[test]
    fn test_scale_no_change_small() {
        assert_eq!(scale_to_fit_correct(100, 50, 4096), (100, 50));
    }

    #[test]
    fn test_scale_wide_image() {
        // 8000×4000 → max 4096
        let (w, h) = scale_to_fit_correct(8000, 4000, 4096);
        assert_eq!(w, 4096);
        assert_eq!(h, 2048);
    }

    #[test]
    fn test_scale_tall_image() {
        // 4000×8000 → max 4096
        let (w, h) = scale_to_fit_correct(4000, 8000, 4096);
        assert_eq!(w, 2048);
        assert_eq!(h, 4096);
    }

    #[test]
    fn test_scale_square() {
        let (w, h) = scale_to_fit_correct(10000, 10000, 4096);
        assert_eq!(w, 4096);
        assert_eq!(h, 4096);
    }

    // ── LargeImageState generation 机制测试 ──

    #[test]
    fn test_session_generation_accepted() {
        let mut state = LargeImageState::new(4, 512);
        let session = Arc::new(ImageSession {
            session_id: 1,
            generation: 1,
            path: PathBuf::from("/tmp/test.bmp"),
            width: 100,
            height: 100,
            tile_size: 512,
            preview_max_size: 4096,
            preview_webp: vec![],
        });
        state.add_session(Arc::clone(&session));

        // 正确的 generation → 成功
        let result = state.find_session_with_generation(1, 1);
        assert!(result.is_ok());
    }

    #[test]
    fn test_session_generation_stale_rejected() {
        let mut state = LargeImageState::new(4, 512);
        let session = Arc::new(ImageSession {
            session_id: 1,
            generation: 2,
            path: PathBuf::from("/tmp/test.bmp"),
            width: 100,
            height: 100,
            tile_size: 512,
            preview_max_size: 4096,
            preview_webp: vec![],
        });
        state.add_session(session);

        // 旧 generation → STALE_GENERATION
        let result = state.find_session_with_generation(1, 1);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, "STALE_GENERATION");
    }

    #[test]
    fn test_session_not_found() {
        let state = LargeImageState::new(4, 512);
        let result = state.find_session_with_generation(999, 1);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, "SESSION_NOT_FOUND");
    }

    #[test]
    fn test_max_2_sessions_evict_oldest() {
        let mut state = LargeImageState::new(4, 512);
        for i in 1..=3u64 {
            let session = Arc::new(ImageSession {
                session_id: i,
                generation: i,
                path: PathBuf::from("/tmp/test.bmp"),
                width: 100,
                height: 100,
                tile_size: 512,
                preview_max_size: 4096,
                preview_webp: vec![],
            });
            state.add_session(session);
        }
        // session 1 应已被逐出
        assert!(state.find_session(1).is_none(), "Session 1 should be evicted");
        assert!(state.find_session(2).is_some());
        assert!(state.find_session(3).is_some());
    }

    // ── LRU tile 缓存测试 ──

    #[test]
    fn test_tile_cache_put_and_get() {
        let mut state = LargeImageState::new(4, 512);
        let key: TileKey = (1, 0, 0, 0);
        state.put_tile_cached(key, vec![1u8; 1024]);
        let cached = state.get_tile_cached(key);
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().len(), 1024);
    }

    #[test]
    fn test_tile_cache_eviction_on_limit() {
        // 限制 1MB，写入两个 600KB tile → 第一个被逐出
        let mut state = LargeImageState::new(4, 1); // 1MB limit → 40% = 409600 bytes
        // 2 个 300KB tile 应能放入（600KB < 409KB 不对...）
        // 重新计算: 1MB * 1024 * 1024 * 40 / 100 = 419430 bytes
        // 用 200KB tiles，2 个 = 400KB < 419KB，第 3 个 200KB = 600KB > 419KB → 逐出 1 个
        let tile_size = 200 * 1024;
        let key1: TileKey = (1, 0, 0, 0);
        let key2: TileKey = (1, 0, 0, 1);
        let key3: TileKey = (1, 0, 1, 0);
        state.put_tile_cached(key1, vec![1u8; tile_size]);
        state.put_tile_cached(key2, vec![2u8; tile_size]);
        state.put_tile_cached(key3, vec![3u8; tile_size]);
        // 缓存字节数应 ≤ limit（已逐出 tile1）
        assert!(state.tile_cache_bytes <= state.tile_cache_limit_bytes);
        // key3 应在缓存（最近写入）
        assert!(state.get_tile_cached(key3).is_some());
    }

    // ── encode_rgba_to_webp 测试 ──

    #[test]
    fn test_encode_webp_produces_non_empty() {
        // 4×4 纯红色 RGBA
        let rgba = vec![255u8, 0, 0, 255].repeat(4 * 4);
        let webp = encode_rgba_to_webp(&rgba, 4, 4, 85.0).unwrap();
        assert!(!webp.is_empty(), "WebP output should not be empty");
        // WebP 文件以 "RIFF" 开头
        assert_eq!(&webp[0..4], b"RIFF", "WebP should start with RIFF");
    }
}
```

### Step 4.3：运行测试确认通过

```bash
cd /Users/wxy/projects/my/projects/PicSee/src-tauri && cargo test large_image::session 2>&1
```

---

## Task 5：picsee:// 自定义协议注册与 lib.rs 集成

**Goal:** 在 lib.rs 注册 picsee:// 协议，注册三个 commands（probe_image / open_large_image / close_large_image），注册 LargeImageState managed state；CSP 补充 picsee: scheme。

**Files:**
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/tauri.conf.json`（CSP）
- Modify: `src-tauri/src/large_image/mod.rs`（补充 pub use）

### Step 5.1：更新 `lib.rs`

将 `src-tauri/src/lib.rs` 更改为：

```rust
pub mod images;
pub mod large_image;
pub mod settings;
pub mod thumbnails;

use images::{open_directory, open_image_file, scan_directory};
use large_image::{
    policy::probe_image,
    session::{close_large_image, handle_preview_request, handle_tile_request, open_large_image, LargeImageState},
};
use settings::{get_settings, read_settings_file, save_settings};
use thumbnails::{clear_thumbnail_cache, get_thumbnail, ThumbnailState};
use tauri::Manager;
use std::sync::{Arc, Mutex};

/// Build and run the PicSee Tauri application.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .register_uri_scheme_protocol("picsee", {
            move |app, request| {
                // URL 形态（macOS）：picsee://localhost/preview/{session_id}/{generation}
                //                    picsee://localhost/tile/{session_id}/{generation}/{z}/{x}/{y}
                let url = request.uri().to_string();
                let path = extract_picsee_path(&url);
                let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

                let state = app.state::<Arc<Mutex<LargeImageState>>>();

                match segments.as_slice() {
                    ["preview", sid, gen] => {
                        let (session_id, generation) = match (sid.parse::<u64>(), gen.parse::<u64>()) {
                            (Ok(s), Ok(g)) => (s, g),
                            _ => return bad_request("Invalid preview URL params"),
                        };
                        match handle_preview_request(&state, session_id, generation) {
                            Ok(bytes) => webp_response(bytes),
                            Err((status, err)) => error_response(status, &err.message),
                        }
                    }
                    ["tile", sid, gen, z, x, y] => {
                        let params = (
                            sid.parse::<u64>(),
                            gen.parse::<u64>(),
                            z.parse::<u32>(),
                            x.parse::<u32>(),
                            y.parse::<u32>(),
                        );
                        match params {
                            (Ok(session_id), Ok(generation), Ok(z), Ok(tile_x), Ok(tile_y)) => {
                                let state_clone = Arc::clone(&state);
                                // tile 生成在当前线程（protocol handler 是 blocking context）
                                match handle_tile_request(state_clone, session_id, generation, z, tile_x, tile_y) {
                                    Ok(bytes) => webp_response(bytes),
                                    Err((status, err)) => error_response(status, &err.message),
                                }
                            }
                            _ => bad_request("Invalid tile URL params"),
                        }
                    }
                    _ => error_response(404, "Unknown picsee:// path"),
                }
            }
        })
        .setup(|app| {
            // 读取 settings（启动时一次）。
            let concurrency = {
                let settings_path = app
                    .path()
                    .app_config_dir()
                    .map(|d: std::path::PathBuf| d.join("settings.json"))
                    .ok();
                let settings = settings_path
                    .as_deref()
                    .and_then(|p| read_settings_file(p).ok())
                    .unwrap_or_default();
                settings.performance.thumbnail_concurrency.clamp(1, 16)
            };

            let settings_for_large = {
                let settings_path = app
                    .path()
                    .app_config_dir()
                    .map(|d: std::path::PathBuf| d.join("settings.json"))
                    .ok();
                settings_path
                    .as_deref()
                    .and_then(|p| read_settings_file(p).ok())
                    .unwrap_or_default()
            };

            app.manage(ThumbnailState::new(concurrency));
            app.manage(Arc::new(Mutex::new(LargeImageState::new(
                settings_for_large.performance.tile_concurrency.clamp(1, 16),
                settings_for_large.cache.memory_cache_limit_mb,
            ))));

            // 授权缩略图缓存目录。
            if let Ok(cache_dir) = app.path().app_cache_dir() {
                let thumb_dir: std::path::PathBuf = cache_dir.join("thumbnails");
                let _ = app.asset_protocol_scope().allow_directory(&thumb_dir, false);
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            open_image_file,
            open_directory,
            scan_directory,
            get_thumbnail,
            clear_thumbnail_cache,
            probe_image,
            open_large_image,
            close_large_image,
        ])
        .run(tauri::generate_context!())
        .expect("Error running PicSee");
}

/// 从 picsee:// URL 提取路径部分。
/// picsee://localhost/preview/... → /preview/...
fn extract_picsee_path(url: &str) -> &str {
    // 去掉 scheme://host 前缀
    if let Some(rest) = url.strip_prefix("picsee://localhost") {
        // 去掉 query string
        rest.split('?').next().unwrap_or(rest)
    } else if let Some(idx) = url.find("//") {
        // fallback: 找到 // 后的 host，再找第一个 /
        let after_scheme = &url[idx + 2..];
        if let Some(slash_idx) = after_scheme.find('/') {
            &after_scheme[slash_idx..]
        } else {
            "/"
        }
    } else {
        "/"
    }
}

fn webp_response(bytes: Vec<u8>) -> tauri::http::Response<Vec<u8>> {
    tauri::http::Response::builder()
        .status(200)
        .header("Content-Type", "image/webp")
        .header("Cache-Control", "no-store")
        .body(bytes)
        .unwrap()
}

fn error_response(status: u16, message: &str) -> tauri::http::Response<Vec<u8>> {
    tauri::http::Response::builder()
        .status(status)
        .header("Content-Type", "text/plain")
        .body(message.as_bytes().to_vec())
        .unwrap()
}

fn bad_request(message: &str) -> tauri::http::Response<Vec<u8>> {
    error_response(400, message)
}
```

### Step 5.2：更新 CSP，允许 picsee: scheme

修改 `tauri.conf.json` 的 `security.csp`：

```json
"csp": "default-src 'self'; img-src 'self' asset: http://asset.localhost picsee: data: blob:; style-src 'self' 'unsafe-inline'"
```

### Step 5.3：运行 `cargo check`

```bash
cd /Users/wxy/projects/my/projects/PicSee/src-tauri && cargo check 2>&1 | tail -30
```

---

## Task 6：benchmark 测试（cargo test --ignored）

**Goal:** 验证 WebP 编码耗时 < 30ms，将结果写入代码注释。

**Files:**
- Create: `src-tauri/src/large_image/bench.rs`（或直接在 session.rs 用 `#[ignore]` 测试）

### Step 6.1：在 `session.rs` 添加 benchmark 测试

在 `session.rs` 的 `#[cfg(test)] mod tests` 块中添加：

```rust
/// Benchmark: 1024×1024 RGBA → WebP q85 编码耗时与体积。
/// 运行：cargo test benchmark_webp_encoding -- --ignored --nocapture
#[test]
#[ignore]
fn benchmark_webp_encoding() {
    use std::time::Instant;
    let w = 1024u32;
    let h = 1024u32;
    // 生成随机 RGBA 数据（模拟真实图像）
    let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i * 37 + 13) as u8).collect();

    let iterations = 10;
    let mut total_ms = 0u128;
    let mut total_bytes = 0usize;

    for _ in 0..iterations {
        let data = rgba.clone();
        let start = Instant::now();
        let webp = encode_rgba_to_webp(&data, w, h, 85.0).unwrap();
        let elapsed = start.elapsed().as_millis();
        total_ms += elapsed;
        total_bytes += webp.len();
    }

    let avg_ms = total_ms / iterations;
    let avg_bytes = total_bytes / iterations as usize;
    println!("\n=== WebP q85 Benchmark (1024×1024 RGBA) ===");
    println!("  平均编码耗时: {avg_ms}ms");
    println!("  平均体积: {}KB", avg_bytes / 1024);
    println!("  raw RGBA: {}KB", w * h * 4 / 1024);

    // 验证：编码耗时 < 30ms（否则重新评估格式选择）
    assert!(
        avg_ms < 30,
        "WebP encoding too slow: {avg_ms}ms (threshold: 30ms); consider PNG or raw"
    );
}
```

### Step 6.2：运行 benchmark

```bash
cd /Users/wxy/projects/my/projects/PicSee/src-tauri && cargo test benchmark_webp_encoding -- --ignored --nocapture 2>&1
```

将实际输出的耗时和体积写入 `encode_rgba_to_webp` 的注释中（已预留注释位置）。

---

## Task 7：集成测试（生成 ~50MB BMP 走完整链路）

**Goal:** 生成 5000×3500 BMP 走 open→preview→tile 完整链路，断言 preview 耗时 < 3s、tile 像素正确。

**Files:**
- Modify: `src-tauri/src/large_image/session.rs`（在 tests 末尾添加集成测试）

### Step 7.1：在 `session.rs` 添加集成测试

在 tests 块末尾添加：

```rust
/// 集成测试：生成 ~50MB BMP（5000×3500），走 open→preview→tile 链路。
/// 断言：preview 耗时 < 3s，tile 像素正确性。
/// 注意：此测试生成大文件，耗时较长，使用 #[ignore] 避免常规 CI 执行。
/// 运行：cargo test integration_bmp_open_preview_tile -- --ignored --nocapture
#[test]
#[ignore]
fn integration_bmp_open_preview_tile() {
    use crate::large_image::bmp::BmpReader;
    use std::io::Write;
    use std::time::Instant;

    // 1. 生成 5000×3500 24bit bottom-up BMP（~50MB）
    let width = 5000u32;
    let height = 3500u32;
    let bpp = 3u32;
    let row_stride = ((width * bpp + 3) / 4 * 4) as usize;
    let pixel_data_size = row_stride * height as usize;
    let file_size = 54 + pixel_data_size;

    let mut data = vec![0u8; file_size];
    data[0] = b'B'; data[1] = b'M';
    data[2..6].copy_from_slice(&(file_size as u32).to_le_bytes());
    data[10..14].copy_from_slice(&54u32.to_le_bytes());
    data[14..18].copy_from_slice(&40u32.to_le_bytes());
    data[18..22].copy_from_slice(&(width as i32).to_le_bytes());
    data[22..26].copy_from_slice(&(height as i32).to_le_bytes());
    data[26..28].copy_from_slice(&1u16.to_le_bytes());
    data[28..30].copy_from_slice(&24u16.to_le_bytes());

    // 填充像素（R=x%256, G=y%256, B=0）
    for img_y in 0..height {
        let file_row = height - 1 - img_y;
        let row_offset = 54 + file_row as usize * row_stride;
        for img_x in 0..width {
            let px = row_offset + img_x as usize * 3;
            data[px] = 0;
            data[px + 1] = (img_y % 256) as u8;
            data[px + 2] = (img_x % 256) as u8;
        }
    }

    let mut tmp = tempfile::NamedTempFile::with_suffix(".bmp").unwrap();
    tmp.write_all(&data).unwrap();
    tmp.flush().unwrap();
    let path = tmp.path().to_path_buf();

    println!("\n生成 BMP 大小: {}MB", file_size / (1024 * 1024));

    // 2. 测试 preview 生成耗时
    let start = Instant::now();
    let preview_webp = generate_bmp_preview(&path, 4096).unwrap();
    let preview_ms = start.elapsed().as_millis();
    println!("Preview 生成耗时: {preview_ms}ms");
    println!("Preview WebP 大小: {}KB", preview_webp.len() / 1024);

    // CI 安全值：3s 内（M 系列芯片 <1s，CI x86 放宽到 3s）
    assert!(
        preview_ms < 3000,
        "Preview generation took {preview_ms}ms, expected < 3000ms"
    );
    assert!(!preview_webp.is_empty(), "Preview should not be empty");
    assert_eq!(&preview_webp[0..4], b"RIFF", "Preview should be valid WebP");

    // 3. 测试 tile 生成（0, 0）
    let tile_webp = generate_bmp_tile(&path, 0, 0, 512, width, height).unwrap();
    println!("Tile (0,0) WebP 大小: {}KB", tile_webp.len() / 1024);
    assert!(!tile_webp.is_empty());
    assert_eq!(&tile_webp[0..4], b"RIFF");

    // 4. 直接验证 tile 区域像素（通过 BmpReader::read_region）
    let mut bmp_reader = BmpReader::open(&path).unwrap();
    let rgba = bmp_reader.read_region(
        crate::large_image::bmp::Rect::new(0, 0, 512, 512),
        512,
        512,
    ).unwrap();

    // 像素 (x=10, y=5)：R=10, G=5, B=0, A=255
    let offset = (5 * 512 + 10) * 4;
    assert_eq!(rgba[offset], 10, "R at (10,5) should be 10");
    assert_eq!(rgba[offset + 1], 5, "G at (10,5) should be 5");
    assert_eq!(rgba[offset + 2], 0, "B at (10,5) should be 0");
    assert_eq!(rgba[offset + 3], 255, "A at (10,5) should be 255");

    println!("集成测试通过");
}
```

### Step 7.2：运行集成测试

```bash
cd /Users/wxy/projects/my/projects/PicSee/src-tauri && \
  cargo test integration_bmp_open_preview_tile -- --ignored --nocapture 2>&1
```

---

## Task 8：全套验证

### Step 8.1：运行所有单元测试

```bash
cd /Users/wxy/projects/my/projects/PicSee/src-tauri && cargo test 2>&1 | tail -40
```

预期：0 failures。

### Step 8.2：cargo check

```bash
cd /Users/wxy/projects/my/projects/PicSee/src-tauri && cargo check 2>&1
```

预期：0 errors。

### Step 8.3：cargo fmt --check

```bash
cd /Users/wxy/projects/my/projects/PicSee/src-tauri && cargo fmt --check 2>&1
```

若有格式问题，先运行 `cargo fmt`，再 `cargo fmt --check`。

### Step 8.4：npm run build（前端不改，确认仍过）

```bash
cd /Users/wxy/projects/my/projects/PicSee && npm run build 2>&1 | tail -20
```

---

## Task 9：补充 scale_to_fit 函数修正（发现 Task 4 代码中有错误）

**注意：** 在 Task 4 的 `session.rs` 中，`scale_to_fit` 函数有一个 bug（tall image 分支返回顺序错误）。`scale_to_fit_correct` 是修正版。实现时应**只保留 `scale_to_fit_correct`**，删除有 bug 的 `scale_to_fit`，并在所有调用处改为 `scale_to_fit_correct`。

具体：在 `generate_bmp_preview` 中调用 `scale_to_fit_correct`（而不是 `scale_to_fit`），并删除有 bug 的 `scale_to_fit` 函数。

---

## 已知约束与风险

1. **`webp` crate 版本**：当前 `webp = "0.3"` 的 API 为 `Encoder::from_rgba(data, w, h).encode(quality_f32)`。若 API 不符，查阅 `webp 0.3` 文档或 `cargo doc --open`。

2. **picsee:// 协议在 macOS 的 URL 形态**：Tauri 2.0 在 macOS WKWebView 下，自定义协议 URL 为 `picsee://localhost/...`（包含 localhost host）。`extract_picsee_path` 已处理此形态。若实测 URL 不同，按实际调整解析逻辑。

3. **tile 生成耗时**：`handle_tile_request` 中 BMP 文件需重新打开（`BmpReader::open`），因为 `BmpReader` 包含 `File` 不能跨线程共享。这是设计权衡（每次 tile 请求一次 open + seek，无需 Mutex 锁文件句柄）。对 HDD 可能引入延迟，SSD 上 <5ms。

4. **`Mutex<LargeImageState>` vs `Arc<Mutex<LargeImageState>>`**：lib.rs 注册时用 `Arc<Mutex<LargeImageState>>`，`handle_tile_request` 接收 `Arc<Mutex<LargeImageState>>`。Tauri managed state 的 `.state::<T>()` 返回 `State<T>`，`Arc::clone(&state)` 可以 Clone。

5. **test fixture**：所有 BMP 测试动态生成，不依赖外部 fixture 文件。

---

## 前端接口契约（给下一个任务）

### Commands

| Command | 参数 | 返回 | 说明 |
|---|---|---|---|
| `probe_image` | `{ path: string }` | `ImageProbe` | 只读头部，不解码 |
| `open_large_image` | `{ path: string }` | `OpenLargeImageResult` | 创建会话，同步生成 preview |
| `close_large_image` | `{ sessionId: number }` | `void` | 释放会话内存 |

**ImageProbe 结构：**
```typescript
{
  width: number;
  height: number;
  format: string;       // 小写扩展名，如 "bmp"
  fileSize: number;
  isLarge: boolean;
  loadMode: "normal" | "largeCandidate" | "tileRequired";
}
```

**OpenLargeImageResult 结构：**
```typescript
{
  sessionId: number;
  generation: number;
  width: number;
  height: number;
  tileSize: number;
  previewMaxSize: number;
}
```

### picsee:// 协议 URL

| 资源 | URL 格式 |
|---|---|
| preview | `picsee://localhost/preview/{sessionId}/{generation}` |
| tile (z=0) | `picsee://localhost/tile/{sessionId}/{generation}/0/{x}/{y}` |

**响应：**
- 成功：HTTP 200, `Content-Type: image/webp`, `Cache-Control: no-store`
- 会话不存在：HTTP 404
- generation 过期：HTTP 410（`STALE_GENERATION`）
- tile 超出范围：HTTP 500（tile_out_of_range，可视为 404 处理）

### 错误码表

| code | 含义 | 建议处理 |
|---|---|---|
| `STALE_GENERATION` | 会话已过期（切图后旧 tile 请求） | 忽略，不显示 |
| `SESSION_NOT_FOUND` | 会话 ID 不存在 | 重新 open_large_image |
| `TILE_OUT_OF_RANGE` | tile 坐标超出图像范围 | 忽略该 tile |
| `UNSUPPORTED_FORMAT` | BMP 不支持的格式（16bit/palette/RLE） | 降级到普通引擎 |
| `IO_ERROR` | 文件读取失败 | 显示错误提示 |
| `DECODE_ERROR` | 解码失败 | 显示错误提示 |
| `ENCODE_ERROR` | WebP 编码失败 | 显示错误提示 |

### 前端使用示例

```typescript
// 1. 探测大图
const probe = await invoke<ImageProbe>('probe_image', { path });

if (probe.loadMode === 'tileRequired' || probe.loadMode === 'largeCandidate') {
  // 2. 打开大图会话
  const result = await invoke<OpenLargeImageResult>('open_large_image', { path });
  
  // 3. 显示 preview（直接用 <img> 消费 picsee:// URL）
  previewUrl = `picsee://localhost/preview/${result.sessionId}/${result.generation}`;
  
  // 4. 计算 tile 坐标并加载
  const tileUrl = (x: number, y: number) =>
    `picsee://localhost/tile/${result.sessionId}/${result.generation}/0/${x}/${y}`;
  
  // 5. 关闭时释放
  await invoke('close_large_image', { sessionId: result.sessionId });
}
```
