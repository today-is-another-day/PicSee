use std::{
    collections::VecDeque,
    num::NonZeroUsize,
    path::Path,
    sync::{Arc, Mutex},
};

use lru::LruCache;
use serde::Serialize;
use tauri::{AppHandle, Manager};
use tokio::sync::Semaphore;

use crate::settings::read_settings_file;

use super::{
    bmp::{BmpReader, Rect},
    LargeImageError,
};

// ─────────────────────────── 数据结构 ───────────────────────────

/// 瓦片缓存键：(session_id, zoom_level, tile_x, tile_y)。
type TileKey = (u64, u32, u32, u32);

/// 单个大图会话。
#[derive(Debug)]
pub struct ImageSession {
    pub session_id: u64,
    pub generation: u64,
    pub path: std::path::PathBuf,
    pub width: u32,
    pub height: u32,
    pub tile_size: u32,
    pub preview_max_size: u32,
    /// 预览图 WebP 字节。
    pub preview_webp: Vec<u8>,
}

/// 全局大图状态（通过 Arc<Mutex<LargeImageState>> 注册为 managed state）。
pub struct LargeImageState {
    /// 最多保留 2 个会话（超出时逐出最旧的）。
    sessions: VecDeque<Arc<ImageSession>>,
    /// 自增生成号，用于防止 stale 请求。
    next_generation: u64,
    /// 瓦片 LRU 缓存。
    tile_cache: LruCache<TileKey, Vec<u8>>,
    /// 当前瓦片缓存占用字节数。
    tile_cache_bytes: usize,
    /// 瓦片缓存上限（字节）。
    tile_cache_limit_bytes: usize,
    /// 并发控制信号量。
    pub semaphore: Arc<Semaphore>,
}

impl LargeImageState {
    /// 构造函数。
    ///
    /// - `tile_concurrency`：最大并发解码数。
    /// - `memory_limit_mb`：内存上限（MB），瓦片缓存占其 40%。
    pub fn new(tile_concurrency: usize, memory_limit_mb: usize) -> Self {
        let tile_cache_limit_bytes = memory_limit_mb * 1024 * 1024 * 40 / 100;
        Self {
            sessions: VecDeque::new(),
            next_generation: 1,
            tile_cache: LruCache::new(NonZeroUsize::new(4096).unwrap()),
            tile_cache_bytes: 0,
            tile_cache_limit_bytes,
            semaphore: Arc::new(Semaphore::new(tile_concurrency)),
        }
    }

    /// 查找会话。
    pub fn find_session(&self, session_id: u64) -> Option<Arc<ImageSession>> {
        self.sessions
            .iter()
            .find(|s| s.session_id == session_id)
            .cloned()
    }

    /// 查找会话并验证 generation（防止 stale 请求）。
    pub fn find_session_with_generation(
        &self,
        session_id: u64,
        generation: u64,
    ) -> Result<Arc<ImageSession>, LargeImageError> {
        match self.find_session(session_id) {
            None => Err(LargeImageError::session_not_found(session_id)),
            Some(s) if s.generation != generation => Err(LargeImageError::stale_generation()),
            Some(s) => Ok(s),
        }
    }

    /// 添加会话；超过 2 个时逐出最旧的。
    pub fn add_session(&mut self, session: Arc<ImageSession>) {
        while self.sessions.len() >= 2 {
            self.sessions.pop_front();
        }
        self.sessions.push_back(session);
    }

    /// 移除指定 session_id 的会话。
    pub fn remove_session(&mut self, session_id: u64) {
        self.sessions.retain(|s| s.session_id != session_id);
    }

    /// 生成并消费下一个 generation。
    pub fn next_generation(&mut self) -> u64 {
        let gen = self.next_generation;
        self.next_generation += 1;
        gen
    }

    /// 从 LRU 缓存中查找瓦片。
    pub fn get_tile_cached(&mut self, key: TileKey) -> Option<Vec<u8>> {
        self.tile_cache.get(&key).cloned()
    }

    /// 写入 LRU 缓存；超限时逐出直到满足限制。
    pub fn put_tile_cached(&mut self, key: TileKey, data: Vec<u8>) {
        // 如果已存在则先减去旧大小
        if let Some(old) = self.tile_cache.peek(&key) {
            self.tile_cache_bytes = self.tile_cache_bytes.saturating_sub(old.len());
        }
        self.tile_cache_bytes += data.len();
        self.tile_cache.put(key, data);

        // 超限时逐出 LRU
        while self.tile_cache_bytes > self.tile_cache_limit_bytes {
            if let Some((_, evicted)) = self.tile_cache.pop_lru() {
                self.tile_cache_bytes = self.tile_cache_bytes.saturating_sub(evicted.len());
            } else {
                break;
            }
        }
    }
}

// ─────────────────────────── 辅助函数 ───────────────────────────

/// open_large_image command 的返回值。
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

/// 等比缩放，最长边 ≤ max。
pub fn scale_to_fit_correct(w: u32, h: u32, max: u32) -> (u32, u32) {
    if w <= max && h <= max {
        return (w, h);
    }
    if w >= h {
        let new_w = max;
        let new_h = (h as u64 * max as u64 / w as u64).max(1) as u32;
        (new_w, new_h)
    } else {
        let new_h = max;
        let new_w = (w as u64 * max as u64 / h as u64).max(1) as u32;
        (new_w, new_h)
    }
}

/// 将 RGBA 字节编码为 WebP。
///
/// # Benchmark 数据（M 系列 Mac，release 模式，2026-06-13 实测）
/// - 1024×1024 RGBA → WebP q80/q85：平均 **24ms**，通过 <30ms 阈值
/// - 1024×1024 RGBA raw：0ms，体积 4096KB
/// - 结论：WebP 编码在 release 模式 <30ms，选 WebP 作为默认格式；
///   debug 模式约 700ms（无优化），仅用于开发期功能测试，不作为性能基准。
pub fn encode_rgba_to_webp(
    rgba: &[u8],
    w: u32,
    h: u32,
    quality: f32,
) -> Result<Vec<u8>, LargeImageError> {
    let encoder = webp::Encoder::from_rgba(rgba, w, h);
    let memory = encoder.encode(quality);
    let bytes = memory.to_vec();
    if bytes.is_empty() {
        return Err(LargeImageError::encode("WebP 编码结果为空"));
    }
    Ok(bytes)
}

/// 生成 BMP 预览图（WebP 字节）。
pub fn generate_bmp_preview(
    path: &Path,
    preview_max_size: u32,
) -> Result<Vec<u8>, LargeImageError> {
    let reader = BmpReader::open(path)?;
    let (pw, ph) = scale_to_fit_correct(reader.info.width, reader.info.height, preview_max_size);
    let rect = Rect {
        x: 0,
        y: 0,
        width: reader.info.width,
        height: reader.info.height,
    };
    let rgba = reader.read_region(rect, pw, ph)?;
    encode_rgba_to_webp(&rgba, pw, ph, 80.0)
}

/// 生成 BMP 瓦片（WebP 字节）。
pub fn generate_bmp_tile(
    path: &Path,
    tx: u32,
    ty: u32,
    tile_size: u32,
    img_w: u32,
    img_h: u32,
) -> Result<Vec<u8>, LargeImageError> {
    let x = tx * tile_size;
    let y = ty * tile_size;
    let w = tile_size.min(img_w.saturating_sub(x));
    let h = tile_size.min(img_h.saturating_sub(y));

    if w == 0 || h == 0 {
        return Err(LargeImageError::tile_out_of_range(tx, ty));
    }

    let reader = BmpReader::open(path)?;
    let rect = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    let rgba = reader.read_region(rect, w, h)?;
    encode_rgba_to_webp(&rgba, w, h, 85.0)
}

/// 生成通用格式（非 BMP）预览图（WebP 字节）。
pub fn generate_generic_preview(path: &Path, preview_max: u32) -> Result<Vec<u8>, LargeImageError> {
    let img =
        image::open(path).map_err(|e| LargeImageError::decode(format!("解码图像失败: {e}")))?;

    let (w, h) = (img.width(), img.height());
    let (pw, ph) = scale_to_fit_correct(w, h, preview_max);

    let thumb = img.thumbnail(pw, ph);
    let rgba = thumb.to_rgba8();
    encode_rgba_to_webp(rgba.as_raw(), thumb.width(), thumb.height(), 80.0)
}

// ─────────────────────────── 协议处理器 ───────────────────────────

/// 将会话查找错误映射为 HTTP 状态码。
/// - STALE_GENERATION（generation 过期）→ 410 Gone
/// - SESSION_NOT_FOUND（会话不存在）→ 404 Not Found
fn session_error_status(err: &LargeImageError) -> u16 {
    match err.code {
        "STALE_GENERATION" => 410,
        _ => 404,
    }
}

/// 处理 picsee://localhost/preview/{session_id}/{generation} 请求。
pub fn handle_preview_request(
    state: &Mutex<LargeImageState>,
    session_id: u64,
    generation: u64,
) -> Result<Vec<u8>, (u16, LargeImageError)> {
    let guard = state.lock().unwrap();
    let session = guard
        .find_session_with_generation(session_id, generation)
        .map_err(|e| (session_error_status(&e), e))?;
    Ok(session.preview_webp.clone())
}

/// 处理 picsee://localhost/tile/{session_id}/{generation}/{z}/{tx}/{ty} 请求。
///
/// 先查 LRU 缓存，命中则直接返回；未命中则在锁外解码，写回缓存。
pub fn handle_tile_request(
    state_arc: Arc<Mutex<LargeImageState>>,
    session_id: u64,
    generation: u64,
    _z: u32,
    tile_x: u32,
    tile_y: u32,
) -> Result<Vec<u8>, (u16, LargeImageError)> {
    let tile_key: TileKey = (session_id, 0, tile_x, tile_y);

    // 先查缓存
    {
        let mut guard = state_arc.lock().unwrap();
        if let Some(cached) = guard.get_tile_cached(tile_key) {
            return Ok(cached);
        }
    }

    // 取出会话信息（锁外解码）
    let (path, tile_size, img_w, img_h) = {
        let guard = state_arc.lock().unwrap();
        let session = guard
            .find_session_with_generation(session_id, generation)
            .map_err(|e| (session_error_status(&e), e))?;
        (
            session.path.clone(),
            session.tile_size,
            session.width,
            session.height,
        )
    };

    // 验证瓦片范围
    let tiles_x = (img_w + tile_size - 1) / tile_size;
    let tiles_y = (img_h + tile_size - 1) / tile_size;
    if tile_x >= tiles_x || tile_y >= tiles_y {
        return Err((400, LargeImageError::tile_out_of_range(tile_x, tile_y)));
    }

    // 生成瓦片（锁外）
    let webp = generate_bmp_tile(&path, tile_x, tile_y, tile_size, img_w, img_h)
        .map_err(|e| (500u16, e))?;

    // 写回缓存
    {
        let mut guard = state_arc.lock().unwrap();
        guard.put_tile_cached(tile_key, webp.clone());
    }

    Ok(webp)
}

// ─────────────────────────── Commands ───────────────────────────

/// 打开大图，创建会话，返回会话信息。
#[tauri::command]
pub async fn open_large_image(
    app: AppHandle,
    path: String,
) -> Result<OpenLargeImageResult, LargeImageError> {
    let settings_path: Option<std::path::PathBuf> = app
        .path()
        .app_config_dir()
        .map(|d| d.join("settings.json"))
        .ok();
    let settings = settings_path
        .as_deref()
        .and_then(|p| read_settings_file(p).ok())
        .unwrap_or_default();

    let tile_size = settings.large_image.tile_size as u32;
    let preview_max_size = settings.large_image.preview_max_size as u32;

    let path_buf = std::path::PathBuf::from(&path);
    let ext = path_buf
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // spawn_blocking：解析 header + 生成 preview
    let path_clone = path_buf.clone();
    #[cfg(debug_assertions)]
    let open_start = std::time::Instant::now();
    let (width, height, preview_webp) = tokio::task::spawn_blocking(move || {
        let (w, h) = if ext == "bmp" {
            use crate::large_image::bmp::BmpInfo;
            let info = BmpInfo::from_file(&path_clone)?;
            (info.width, info.height)
        } else {
            let reader = image::ImageReader::open(&path_clone)
                .map_err(|e| LargeImageError::io(format!("打开图像失败: {e}")))?
                .with_guessed_format()
                .map_err(|e| LargeImageError::io(format!("猜测格式失败: {e}")))?;
            reader
                .into_dimensions()
                .map_err(|e| LargeImageError::decode(format!("读取尺寸失败: {e}")))?
        };

        let preview = if ext == "bmp" {
            generate_bmp_preview(&path_clone, preview_max_size)?
        } else {
            generate_generic_preview(&path_clone, preview_max_size)?
        };

        Ok::<_, LargeImageError>((w, h, preview))
    })
    .await
    .map_err(|e| LargeImageError::io(format!("spawn_blocking 失败: {e}")))??;

    #[cfg(debug_assertions)]
    println!("[PicSee] open_large_image: {}×{}, preview_gen耗时={}ms",
        width, height, open_start.elapsed().as_millis());

    // 注册到 managed state
    let state_arc = app.state::<Arc<Mutex<LargeImageState>>>().inner().clone();
    let mut guard = state_arc.lock().unwrap();
    let generation = guard.next_generation();
    let session = Arc::new(ImageSession {
        session_id: generation,
        generation,
        path: path_buf,
        width,
        height,
        tile_size,
        preview_max_size,
        preview_webp,
    });
    let result = OpenLargeImageResult {
        session_id: session.session_id,
        generation: session.generation,
        width,
        height,
        tile_size,
        preview_max_size,
    };
    guard.add_session(session);

    Ok(result)
}

/// 关闭大图会话。
#[tauri::command]
pub async fn close_large_image(app: AppHandle, session_id: u64) -> Result<(), LargeImageError> {
    let state_arc = app.state::<Arc<Mutex<LargeImageState>>>().inner().clone();
    let mut guard = state_arc.lock().unwrap();
    guard.remove_session(session_id);
    Ok(())
}

// ─────────────────────────── 测试 ───────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use std::io::Write;
    use tempfile::NamedTempFile;

    // ── scale_to_fit_correct ──

    #[test]
    fn test_scale_no_change_small() {
        assert_eq!(scale_to_fit_correct(100, 100, 4096), (100, 100));
    }

    #[test]
    fn test_scale_wide_image() {
        // 8000×4000 → 4096×2048
        assert_eq!(scale_to_fit_correct(8000, 4000, 4096), (4096, 2048));
    }

    #[test]
    fn test_scale_tall_image() {
        // 4000×8000 → 2048×4096
        assert_eq!(scale_to_fit_correct(4000, 8000, 4096), (2048, 4096));
    }

    #[test]
    fn test_scale_square() {
        // 8192×8192 → 4096×4096
        assert_eq!(scale_to_fit_correct(8192, 8192, 4096), (4096, 4096));
    }

    // ── session generation ──

    fn make_state() -> LargeImageState {
        LargeImageState::new(4, 512)
    }

    fn make_session(id: u64) -> Arc<ImageSession> {
        Arc::new(ImageSession {
            session_id: id,
            generation: id,
            path: std::path::PathBuf::from("/tmp/test.bmp"),
            width: 1000,
            height: 1000,
            tile_size: 512,
            preview_max_size: 4096,
            preview_webp: vec![],
        })
    }

    #[test]
    fn test_session_generation_accepted() {
        let mut state = make_state();
        state.add_session(make_session(1));
        let result = state.find_session_with_generation(1, 1);
        assert!(result.is_ok());
    }

    #[test]
    fn test_session_generation_stale_rejected() {
        let mut state = make_state();
        state.add_session(make_session(1));
        let result = state.find_session_with_generation(1, 99);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, "STALE_GENERATION");
    }

    #[test]
    fn test_session_not_found() {
        let state = make_state();
        let result = state.find_session_with_generation(999, 999);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, "SESSION_NOT_FOUND");
    }

    #[test]
    fn test_max_2_sessions_evict_oldest() {
        let mut state = make_state();
        state.add_session(make_session(1));
        state.add_session(make_session(2));
        state.add_session(make_session(3));
        // session 1 应已被逐出
        assert!(state.find_session(1).is_none());
        assert!(state.find_session(2).is_some());
        assert!(state.find_session(3).is_some());
    }

    // ── tile cache ──

    #[test]
    fn test_tile_cache_put_and_get() {
        let mut state = make_state();
        let key: TileKey = (1, 0, 0, 0);
        let data = vec![1u8, 2, 3];
        state.put_tile_cached(key, data.clone());
        assert_eq!(state.get_tile_cached(key), Some(data));
    }

    #[test]
    fn test_tile_cache_eviction_on_limit() {
        // 内存限制 1MB，40% = 409600 字节
        let mut state = LargeImageState::new(4, 1);
        // 每个瓦片 200KB，放 3 个，应触发逐出
        let tile_data = vec![0u8; 200 * 1024];
        state.put_tile_cached((1, 0, 0, 0), tile_data.clone());
        state.put_tile_cached((1, 0, 1, 0), tile_data.clone());
        state.put_tile_cached((1, 0, 2, 0), tile_data.clone());
        // 3 * 200KB = 600KB > 409KB，最旧的应被逐出
        assert!(state.tile_cache_bytes <= state.tile_cache_limit_bytes);
    }

    // ── WebP 编码 ──

    #[test]
    fn test_encode_webp_produces_non_empty() {
        // 4×4 纯红 RGBA
        let rgba = vec![255u8, 0, 0, 255].repeat(4 * 4);
        let result = encode_rgba_to_webp(&rgba, 4, 4, 80.0).unwrap();
        // 验证 RIFF 头
        assert!(result.len() >= 4);
        assert_eq!(&result[0..4], b"RIFF");
    }

    // ── 基准测试（#[ignore]）──

    /// WebP 编码 benchmark。
    ///
    /// 运行：`cargo test --release benchmark_webp_encoding -- --ignored --nocapture`
    ///
    /// **必须用 release 模式运行**：debug 模式无优化，编码约 700ms（不代表真实性能）。
    /// release 模式实测 24ms（M 系列 Mac，2026-06-13），通过 <30ms 阈值，选 WebP 作默认格式。
    #[test]
    #[ignore]
    fn benchmark_webp_encoding() {
        // 1024×1024 RGBA，10 次迭代
        let rgba = vec![128u8; 1024 * 1024 * 4];
        let iterations = 10;
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            encode_rgba_to_webp(&rgba, 1024, 1024, 80.0).unwrap();
        }
        let total_ms = start.elapsed().as_millis();
        let avg_ms = total_ms / iterations as u128;
        println!("WebP 编码平均耗时: {avg_ms}ms");

        // 只在 release 模式断言性能阈值；debug 模式仅打印数据（无优化，约 700ms）。
        #[cfg(not(debug_assertions))]
        assert!(
            avg_ms < 30,
            "平均编码时间 {avg_ms}ms 超过 30ms 预算（release 模式基准）"
        );
        #[cfg(debug_assertions)]
        println!("（debug 模式：编码未优化，性能数据仅供参考，非基准）");
    }

    // ── 集成测试（#[ignore]）──

    #[test]
    #[ignore]
    fn integration_bmp_open_preview_tile() {
        let width: u32 = 5000;
        let height: u32 = 3500;
        let bpp: u32 = 3;
        let row_stride = (width * bpp + 3) & !3;
        let pixel_data_size = row_stride * height;
        let file_size = 54 + pixel_data_size;

        let mut data = vec![0u8; file_size as usize];
        data[0] = b'B';
        data[1] = b'M';
        data[2..6].copy_from_slice(&file_size.to_le_bytes());
        data[10..14].copy_from_slice(&54u32.to_le_bytes());
        data[14..18].copy_from_slice(&40u32.to_le_bytes());
        data[18..22].copy_from_slice(&(width as i32).to_le_bytes());
        data[22..26].copy_from_slice(&(height as i32).to_le_bytes());
        data[26..28].copy_from_slice(&1u16.to_le_bytes());
        data[28..30].copy_from_slice(&24u16.to_le_bytes());

        // 填充像素
        for img_y in 0..height {
            let file_row = height - 1 - img_y;
            let row_start = 54 + file_row as usize * row_stride as usize;
            for img_x in 0..width {
                let off = row_start + img_x as usize * 3;
                data[off] = (img_y % 256) as u8;
                data[off + 1] = (img_x % 128) as u8;
                data[off + 2] = 100;
            }
        }

        let mut f = NamedTempFile::new().unwrap();
        f.write_all(&data).unwrap();
        f.flush().unwrap();

        let start = std::time::Instant::now();
        let preview = generate_bmp_preview(f.path(), 4096).unwrap();
        let preview_ms = start.elapsed().as_millis();
        println!(
            "Preview 生成耗时: {preview_ms}ms，大小: {}KB",
            preview.len() / 1024
        );
        assert!(preview_ms < 3000, "预览生成超时: {preview_ms}ms");
        assert_eq!(&preview[0..4], b"RIFF");

        // 验证瓦片
        let tile = generate_bmp_tile(f.path(), 0, 0, 512, width, height).unwrap();
        assert!(!tile.is_empty());
        println!("Tile (0,0) 大小: {}KB", tile.len() / 1024);
    }
}
