use serde::Serialize;
use std::{fs, path::Path};
use tauri::AppHandle;

use crate::extended_formats;
use crate::settings::LargeImageSettings;

use super::LargeImageError;

/// 图像加载模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LoadMode {
    /// 直接加载，无需特殊处理。
    Normal,
    /// 大图候选，建议降分辨率预览。
    LargeCandidate,
    /// 必须分块加载。
    TileRequired,
}

/// 图像探测结果。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageProbe {
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub file_size: u64,
    pub is_large: bool,
    pub load_mode: LoadMode,
    pub tileable: bool,
    pub raw_preview: bool,
}

/// 判断图像是否为大图。
pub fn is_large_image(
    w: u32,
    h: u32,
    file_size: u64,
    ext: &str,
    settings: &LargeImageSettings,
) -> bool {
    let pixel_count = w as u64 * h as u64;
    let file_size_mb = file_size / (1024 * 1024);
    let ext_lower = ext.to_lowercase();

    if pixel_count >= settings.pixel_threshold {
        return true;
    }
    if file_size_mb >= settings.file_size_threshold_mb {
        return true;
    }
    if w >= settings.side_threshold || h >= settings.side_threshold {
        return true;
    }
    // BMP 超过 100MB 也视为大图
    if ext_lower == "bmp" && file_size_mb >= 100 {
        return true;
    }
    false
}

/// 根据图像属性决定加载模式。
pub fn determine_load_mode(
    w: u32,
    h: u32,
    file_size: u64,
    ext: &str,
    settings: &LargeImageSettings,
) -> LoadMode {
    let pixel_count = w as u64 * h as u64;
    let file_size_mb = file_size / (1024 * 1024);
    let ext_lower = ext.to_lowercase();

    // BMP 超 300MB → TileRequired
    if ext_lower == "bmp" && file_size_mb >= 300 {
        return LoadMode::TileRequired;
    }
    // BMP 100-300MB → LargeCandidate
    if ext_lower == "bmp" && file_size_mb >= 100 {
        return LoadMode::LargeCandidate;
    }
    // 像素总量超阈值 → TileRequired
    if pixel_count >= settings.pixel_threshold {
        return LoadMode::TileRequired;
    }
    // 文件大小超阈值 → TileRequired
    if file_size_mb >= settings.file_size_threshold_mb {
        return LoadMode::TileRequired;
    }
    LoadMode::Normal
}

/// 探测图像文件，返回 ImageProbe。
pub fn probe_image_file(
    path: &Path,
    settings: &LargeImageSettings,
) -> Result<ImageProbe, LargeImageError> {
    let metadata =
        fs::metadata(path).map_err(|e| LargeImageError::io(format!("无法获取文件元数据: {e}")))?;
    let file_size = metadata.len();

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let (width, height, format, tileable, raw_preview) = if ext == "bmp" {
        let info = super::bmp::BmpInfo::from_file(path)?;
        (info.width, info.height, "bmp".to_string(), true, false)
    } else if extended_formats::is_system_decoded(path) {
        let (w, h) =
            extended_formats::probe_system_image(path).map_err(LargeImageError::system_decode)?;
        (w, h, ext.clone(), false, extended_formats::is_raw(path))
    } else {
        let reader = image::ImageReader::open(path)
            .map_err(|e| LargeImageError::io(format!("打开图像失败: {e}")))?
            .with_guessed_format()
            .map_err(|e| LargeImageError::io(format!("猜测格式失败: {e}")))?;
        let fmt = reader
            .format()
            .map(|f| format!("{f:?}").to_lowercase())
            .unwrap_or_else(|| ext.clone());
        let (w, h) = reader
            .into_dimensions()
            .map_err(|e| LargeImageError::decode(format!("读取图像尺寸失败: {e}")))?;
        (w, h, fmt, false, false)
    };

    let is_large = is_large_image(width, height, file_size, &ext, settings);
    let load_mode = if extended_formats::is_system_decoded(path) {
        LoadMode::LargeCandidate
    } else {
        determine_load_mode(width, height, file_size, &ext, settings)
    };

    Ok(ImageProbe {
        width,
        height,
        format,
        file_size,
        is_large,
        load_mode,
        tileable,
        raw_preview,
    })
}

/// 探测图像文件（Tauri command）。
#[tauri::command]
pub async fn probe_image(app: AppHandle, path: String) -> Result<ImageProbe, LargeImageError> {
    use crate::settings::read_settings_file;
    use tauri::Manager;

    let settings_path: Option<std::path::PathBuf> = app
        .path()
        .app_config_dir()
        .map(|d| d.join("settings.json"))
        .ok();
    let settings = settings_path
        .as_deref()
        .and_then(|p| read_settings_file(p).ok())
        .unwrap_or_default();
    let large_settings = settings.large_image;

    #[cfg(debug_assertions)]
    let probe_start = std::time::Instant::now();
    let result =
        tokio::task::spawn_blocking(move || probe_image_file(Path::new(&path), &large_settings))
            .await
            .map_err(|e| LargeImageError::io(format!("spawn_blocking 失败: {e}")))?;
    #[cfg(debug_assertions)]
    if let Ok(ref probe) = result {
        println!(
            "[PicSee] probe_image: loadMode={:?} {}×{} 耗时={}ms",
            probe.load_mode,
            probe.width,
            probe.height,
            probe_start.elapsed().as_millis()
        );
    }
    result
}

// ─────────────────────────── 测试 ───────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::Command;
    use tempfile::NamedTempFile;

    fn default_settings() -> LargeImageSettings {
        LargeImageSettings::default()
    }

    fn make_system_decoded_file(extension: &str) -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        let png = directory.path().join("source.png");
        let output = directory.path().join(format!("source.{extension}"));
        image::DynamicImage::new_rgb8(8, 6).save(&png).unwrap();
        assert!(Command::new("sips")
            .args(["-s", "format", "tiff"])
            .arg(&png)
            .args(["--out"])
            .arg(&output)
            .output()
            .unwrap()
            .status
            .success());
        directory
    }

    // ── is_large_image ──

    #[test]
    fn test_not_large_below_all_thresholds() {
        let s = default_settings();
        // 1000×1000 = 1M pixels, 10MB, no large side
        assert!(!is_large_image(1000, 1000, 10 * 1024 * 1024, "png", &s));
    }

    #[test]
    fn test_large_by_pixel_threshold() {
        let s = default_settings();
        // 7072 × 7072 ≈ 50M pixels (>= 50_000_000)
        assert!(is_large_image(7072, 7072, 10 * 1024 * 1024, "png", &s));
    }

    #[test]
    fn test_large_by_file_size_threshold() {
        let s = default_settings();
        // 301MB 文件
        assert!(is_large_image(1000, 1000, 301 * 1024 * 1024, "png", &s));
    }

    #[test]
    fn test_large_by_side_threshold() {
        let s = default_settings();
        // width == side_threshold (12_000)
        assert!(is_large_image(12000, 100, 1 * 1024 * 1024, "png", &s));
    }

    #[test]
    fn test_bmp_over_100mb_is_large() {
        let s = default_settings();
        // BMP 101MB，像素和边长都不超阈值
        assert!(is_large_image(1000, 1000, 101 * 1024 * 1024, "bmp", &s));
    }

    // ── determine_load_mode ──

    #[test]
    fn test_load_mode_normal() {
        let s = default_settings();
        assert_eq!(
            determine_load_mode(1000, 1000, 10 * 1024 * 1024, "png", &s),
            LoadMode::Normal
        );
    }

    #[test]
    fn test_load_mode_bmp_over_300mb() {
        let s = default_settings();
        assert_eq!(
            determine_load_mode(1000, 1000, 301 * 1024 * 1024, "bmp", &s),
            LoadMode::TileRequired
        );
    }

    #[test]
    fn test_load_mode_bmp_100_300mb() {
        let s = default_settings();
        // 200MB BMP → LargeCandidate
        assert_eq!(
            determine_load_mode(1000, 1000, 200 * 1024 * 1024, "bmp", &s),
            LoadMode::LargeCandidate
        );
    }

    #[test]
    fn test_load_mode_tile_required_by_pixels() {
        let s = default_settings();
        // 7072 × 7072 ≈ 50M pixels
        assert_eq!(
            determine_load_mode(7072, 7072, 10 * 1024 * 1024, "png", &s),
            LoadMode::TileRequired
        );
    }

    // ── probe_bmp_dimensions ──

    /// 生成最小有效 BMP 头（54 字节），不含像素数据。
    fn make_bmp_header(width: i32, height: i32) -> Vec<u8> {
        let mut h = vec![0u8; 54];
        // BM 魔数
        h[0] = b'B';
        h[1] = b'M';
        // 文件大小（简化，不重要）
        let file_size: u32 = 54;
        h[2..6].copy_from_slice(&file_size.to_le_bytes());
        // 像素数据偏移
        h[10..14].copy_from_slice(&54u32.to_le_bytes());
        // DIB header size = 40
        h[14..18].copy_from_slice(&40u32.to_le_bytes());
        // width
        h[18..22].copy_from_slice(&width.to_le_bytes());
        // height（正数 = bottom-up）
        h[22..26].copy_from_slice(&height.to_le_bytes());
        // planes = 1
        h[26..28].copy_from_slice(&1u16.to_le_bytes());
        // bit count = 24
        h[28..30].copy_from_slice(&24u16.to_le_bytes());
        h
    }

    #[test]
    fn test_probe_bmp_bottom_up() {
        let header = make_bmp_header(100, 80);
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(&header).unwrap();
        f.flush().unwrap();

        let info = super::super::bmp::BmpInfo::from_file(f.path()).unwrap();
        assert_eq!(info.width, 100);
        assert_eq!(info.height, 80);
    }

    #[test]
    fn test_probe_bmp_top_down_negative_height() {
        // 负高度 = top-down，应取绝对值
        let header = make_bmp_header(200, -150);
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(&header).unwrap();
        f.flush().unwrap();

        let info = super::super::bmp::BmpInfo::from_file(f.path()).unwrap();
        assert_eq!(info.width, 200);
        assert_eq!(info.height, 150);
    }

    #[test]
    fn test_probe_bmp_bad_magic() {
        let mut header = make_bmp_header(100, 80);
        header[0] = b'X'; // 破坏魔数
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(&header).unwrap();
        f.flush().unwrap();

        let result = super::super::bmp::BmpInfo::from_file(f.path());
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, "DECODE_ERROR");
    }

    #[test]
    fn test_probe_tiff_is_preview_only() {
        let directory = make_system_decoded_file("tiff");
        let probe = probe_image_file(&directory.path().join("source.tiff"), &default_settings())
            .expect("TIFF 应由系统解码器探测");
        assert_eq!(probe.load_mode, LoadMode::LargeCandidate);
        assert!(!probe.tileable);
        assert!(!probe.raw_preview);
    }

    #[test]
    fn test_probe_raw_is_preview_only() {
        let directory = make_system_decoded_file("dng");
        let probe = probe_image_file(&directory.path().join("source.dng"), &default_settings())
            .expect("RAW embedded preview 应由系统解码器探测");
        assert_eq!(probe.load_mode, LoadMode::LargeCandidate);
        assert!(!probe.tileable);
        assert!(probe.raw_preview);
    }
}
