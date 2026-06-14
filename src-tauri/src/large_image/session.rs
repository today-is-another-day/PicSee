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

use crate::extended_formats;
use crate::settings::read_settings_file;

use super::{
    bmp::{BmpReader, Rect},
    LargeImageError,
};

// ─────────────────────────── 数据结构 ───────────────────────────

/// 瓦片缓存键：(session_id, zoom_level, tile_x, tile_y)。
type TileKey = (u64, u32, u32, u32);

/// 常驻预览（原始 RGBA）的最长边上限，控制内存占用。
/// 清晰度由瓦片保证，故基础预览不需要太大；2048 边长 ≈ 14MB/会话。
const PREVIEW_RAW_CAP: u32 = 2048;

/// 导航窗预览最长边上限（小图，供 NavigatorOverlay 的 <img> 显示）。
const NAV_PREVIEW_CAP: u32 = 384;

/// 非 BMP 大图落临时栅格的像素上限：超过则退化为仅预览，避免一次性解码 OOM。
/// 100M 像素 ≈ 解码峰值数百 MB，权衡内存预算与可视化收益。
const MAX_RASTER_PIXELS: u64 = 100_000_000;

/// 临时栅格文件序号（保证文件名唯一）。
static RASTER_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 单个大图会话。
#[derive(Debug)]
pub struct ImageSession {
    pub session_id: u64,
    pub path: std::path::PathBuf,
    pub width: u32,
    pub height: u32,
    pub tile_size: u32,
    pub preview_max_size: u32,
    /// 是否支持原始分辨率瓦片读取；M4 仅支持 24/32-bit BI_RGB BMP。
    pub tileable: bool,
    pub raw_preview: bool,
    /// 瓦片读取的源文件：BMP 为原文件，非 BMP 为解码后落盘的临时 32-bit BMP 栅格。
    pub tile_source_path: std::path::PathBuf,
    /// tile_source_path 是否为本引擎生成的临时栅格（会话关闭时删除）。
    pub tile_source_is_temp: bool,
    /// 预览图原始 RGBA 字节（不编码，前端用 ImageData 渲染，避免 WebP 编码耗时）。
    /// 像素尺寸通过 OpenLargeImageResult.preview_w/h 告知前端。
    pub preview_rgba: Vec<u8>,
    /// 导航窗用的小 WebP（供 NavigatorOverlay 的 <img> 显示，不能用原始 RGBA）。
    pub nav_preview_webp: Vec<u8>,
}

/// 全局大图状态（通过 Arc<Mutex<LargeImageState>> 注册为 managed state）。
pub struct LargeImageState {
    /// 最多保留 2 个会话（超出时逐出最旧的）。
    sessions: VecDeque<Arc<ImageSession>>,
    /// 自增会话 ID。
    next_session_id: u64,
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
            next_session_id: 1,
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

    /// 添加会话；超过 2 个时逐出最旧的。返回被逐出会话的临时栅格路径（供调用方删除）。
    pub fn add_session(&mut self, session: Arc<ImageSession>) -> Vec<std::path::PathBuf> {
        let mut evicted_temps = Vec::new();
        while self.sessions.len() >= 2 {
            if let Some(evicted) = self.sessions.pop_front() {
                self.clear_session_tiles(evicted.session_id);
                if evicted.tile_source_is_temp {
                    evicted_temps.push(evicted.tile_source_path.clone());
                }
            }
        }
        self.sessions.push_back(session);
        evicted_temps
    }

    /// 移除指定 session_id 的会话。返回其临时栅格路径（若有，供调用方删除）。
    pub fn remove_session(&mut self, session_id: u64) -> Option<std::path::PathBuf> {
        let temp = self
            .sessions
            .iter()
            .find(|s| s.session_id == session_id)
            .filter(|s| s.tile_source_is_temp)
            .map(|s| s.tile_source_path.clone());
        self.sessions.retain(|s| s.session_id != session_id);
        self.clear_session_tiles(session_id);
        temp
    }

    /// 生成并消费下一个会话 ID。
    pub fn next_session_id(&mut self) -> u64 {
        let id = self.next_session_id;
        self.next_session_id += 1;
        id
    }

    /// 清理指定会话的全部瓦片缓存并校正字节计数。
    fn clear_session_tiles(&mut self, session_id: u64) {
        let keys: Vec<TileKey> = self
            .tile_cache
            .iter()
            .filter_map(|(key, _)| (key.0 == session_id).then_some(*key))
            .collect();
        for key in keys {
            if let Some(data) = self.tile_cache.pop(&key) {
                self.tile_cache_bytes = self.tile_cache_bytes.saturating_sub(data.len());
            }
        }
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
    pub width: u32,
    pub height: u32,
    pub tile_size: u32,
    pub preview_max_size: u32,
    pub tileable: bool,
    pub raw_preview: bool,
    /// 预览图像素宽（前端按此尺寸构造 ImageData）。
    pub preview_w: u32,
    /// 预览图像素高。
    pub preview_h: u32,
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

/// 生成 BMP 预览图（原始 RGBA 字节 + 尺寸）。
pub fn generate_bmp_preview(
    path: &Path,
    preview_max_size: u32,
    threads: u32,
) -> Result<(Vec<u8>, u32, u32), LargeImageError> {
    let reader = BmpReader::open(path)?;
    let cap = preview_max_size.min(PREVIEW_RAW_CAP);
    let (pw, ph) = scale_to_fit_correct(reader.info.width, reader.info.height, cap);
    let rect = Rect {
        x: 0,
        y: 0,
        width: reader.info.width,
        height: reader.info.height,
    };
    let rgba = reader.read_region_parallel(rect, pw, ph, threads)?;
    Ok((rgba, pw, ph))
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

/// 生成通用格式（非 BMP）预览图（原始 RGBA 字节 + 尺寸）。
pub fn generate_generic_preview(
    path: &Path,
    preview_max: u32,
) -> Result<(Vec<u8>, u32, u32), LargeImageError> {
    let img = if extended_formats::needs_colorsync_output(path) {
        extended_formats::decode_system_image(path).map_err(LargeImageError::system_decode)?
    } else {
        image::open(path).map_err(|e| LargeImageError::decode(format!("解码图像失败: {e}")))?
    };

    let (w, h) = (img.width(), img.height());
    let cap = preview_max.min(PREVIEW_RAW_CAP);
    let (pw, ph) = scale_to_fit_correct(w, h, cap);

    let thumb = img.thumbnail(pw, ph);
    // 缩略后立即释放原始缓冲。
    drop(img);
    let rgba = thumb.to_rgba8();
    Ok((rgba.into_raw(), thumb.width(), thumb.height()))
}

// ─────────────────────────── 协议处理器 ───────────────────────────

/// 最近邻把 RGBA 缩到最长边 ≤ cap，返回 (rgba, w, h)。
fn downscale_rgba(src: &[u8], sw: u32, sh: u32, cap: u32) -> (Vec<u8>, u32, u32) {
    let (nw, nh) = scale_to_fit_correct(sw, sh, cap);
    if nw == sw && nh == sh {
        return (src.to_vec(), sw, sh);
    }
    let mut out = vec![0u8; nw as usize * nh as usize * 4];
    for ty in 0..nh {
        let sy = (ty as u64 * sh as u64 / nh as u64) as u32;
        for tx in 0..nw {
            let sx = (tx as u64 * sw as u64 / nw as u64) as u32;
            let si = (sy as usize * sw as usize + sx as usize) * 4;
            let di = (ty as usize * nw as usize + tx as usize) * 4;
            out[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
    (out, nw, nh)
}

/// 从已解码图像快速生成预览 RGBA（最长边 ≤ cap），直接最近邻采样源缓冲。
///
/// 比 `DynamicImage::thumbnail`（多遍高质量缩放，需遍历全部源像素）快得多：
/// 只读取输出像素数量的源像素（带步进），且 Rgb8/Rgba8 直接采样、不做整图 RGBA 转换。
fn fast_preview_rgba(img: &image::DynamicImage, cap: u32) -> (Vec<u8>, u32, u32) {
    use image::DynamicImage::{ImageRgb8, ImageRgba8};
    let (sw, sh) = (img.width(), img.height());
    let (pw, ph) = scale_to_fit_correct(sw, sh, cap);

    let sample = |raw: &[u8], ch: usize| {
        let mut out = vec![0u8; pw as usize * ph as usize * 4];
        for ty in 0..ph as usize {
            let sy = (ty as u64 * sh as u64 / ph as u64) as usize;
            let row = sy * sw as usize * ch;
            for tx in 0..pw as usize {
                let sx = (tx as u64 * sw as u64 / pw as u64) as usize;
                let s = row + sx * ch;
                let d = (ty * pw as usize + tx) * 4;
                out[d] = raw[s];
                out[d + 1] = raw[s + 1];
                out[d + 2] = raw[s + 2];
                out[d + 3] = if ch == 4 { raw[s + 3] } else { 255 };
            }
        }
        out
    };

    let out = match img {
        ImageRgb8(buf) => sample(buf.as_raw(), 3),
        ImageRgba8(buf) => sample(buf.as_raw(), 4),
        // 其它色彩类型较少见：转 RGBA 后再采样（一次性，体量小图才会到这里）。
        other => sample(other.to_rgba8().as_raw(), 4),
    };
    (out, pw, ph)
}

/// 从预览 RGBA 生成导航窗用的小 WebP（最长边 ≤ NAV_PREVIEW_CAP，编码极快）。
pub fn make_nav_preview(preview_rgba: &[u8], pw: u32, ph: u32) -> Result<Vec<u8>, LargeImageError> {
    let (small, nw, nh) = downscale_rgba(preview_rgba, pw, ph, NAV_PREVIEW_CAP);
    encode_rgba_to_webp(&small, nw, nh, 80.0)
}

/// 把 RGB(3) 或 RGBA(4) 缓冲写成 top-down BI_RGB BMP 临时文件，供 `BmpReader` 区域分块复用。
/// 行式写入，内存只多一个行缓冲（不复制整图）。3 通道写 24-bit、4 通道写 32-bit，
/// 24-bit 行按 4 字节对齐填充（与 BmpReader 的 row_stride 一致）。
fn write_temp_bmp_raster(
    src: &[u8],
    channels: usize,
    w: u32,
    h: u32,
    dst: &Path,
) -> Result<(), LargeImageError> {
    use std::io::{BufWriter, Write};
    debug_assert!(channels == 3 || channels == 4);
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| LargeImageError::io(format!("创建栅格目录失败: {e}")))?;
    }
    let src_row = w as usize * channels;
    // 目标 BMP 每行按 4 字节对齐（24-bit 需要填充，32-bit 天然对齐）。
    let dst_row = (w as usize * channels + 3) & !3;
    let file_size = 54u64 + dst_row as u64 * h as u64;

    let file =
        std::fs::File::create(dst).map_err(|e| LargeImageError::io(format!("创建栅格失败: {e}")))?;
    let mut writer = BufWriter::new(file);

    // BITMAPINFOHEADER（54 字节）：top-down（高度取负）、BI_RGB。
    let mut hdr = [0u8; 54];
    hdr[0] = b'B';
    hdr[1] = b'M';
    hdr[2..6].copy_from_slice(&(file_size.min(u32::MAX as u64) as u32).to_le_bytes());
    hdr[10..14].copy_from_slice(&54u32.to_le_bytes());
    hdr[14..18].copy_from_slice(&40u32.to_le_bytes());
    hdr[18..22].copy_from_slice(&(w as i32).to_le_bytes());
    hdr[22..26].copy_from_slice(&(-(h as i32)).to_le_bytes());
    hdr[26..28].copy_from_slice(&1u16.to_le_bytes());
    hdr[28..30].copy_from_slice(&((channels as u16) * 8).to_le_bytes());
    hdr[30..34].copy_from_slice(&0u32.to_le_bytes());
    writer
        .write_all(&hdr)
        .map_err(|e| LargeImageError::io(format!("写栅格头失败: {e}")))?;

    // 像素：RGB(A) → BGR(A)，逐行写（top-down，行 0 在前），尾部补对齐填充。
    let mut row = vec![0u8; dst_row];
    for y in 0..h as usize {
        let s0 = y * src_row;
        let line = &src[s0..s0 + src_row];
        for x in 0..w as usize {
            let s = x * channels;
            let d = x * channels;
            row[d] = line[s + 2]; // B
            row[d + 1] = line[s + 1]; // G
            row[d + 2] = line[s]; // R
            if channels == 4 {
                row[d + 3] = line[s + 3]; // A
            }
        }
        writer
            .write_all(&row)
            .map_err(|e| LargeImageError::io(format!("写栅格行失败: {e}")))?;
    }
    writer
        .flush()
        .map_err(|e| LargeImageError::io(format!("flush 栅格失败: {e}")))?;
    Ok(())
}

/// 处理 picsee://localhost/preview/{session_id} 请求（返回导航窗小 WebP）。
pub fn handle_preview_request(
    state: &Mutex<LargeImageState>,
    session_id: u64,
) -> Result<Vec<u8>, (u16, LargeImageError)> {
    let guard = state.lock().unwrap();
    let session = guard
        .find_session(session_id)
        .ok_or_else(|| (404, LargeImageError::session_not_found(session_id)))?;
    Ok(session.nav_preview_webp.clone())
}

/// 处理 picsee://localhost/tile/{session_id}/{z}/{tx}/{ty} 请求。
///
/// 先查 LRU 缓存，命中则直接返回；未命中则在锁外解码，写回缓存。
pub fn handle_tile_request(
    state_arc: Arc<Mutex<LargeImageState>>,
    session_id: u64,
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
            .find_session(session_id)
            .ok_or_else(|| (404, LargeImageError::session_not_found(session_id)))?;
        if !session.tileable {
            return Err((415, LargeImageError::tiles_unavailable()));
        }
        (
            session.tile_source_path.clone(),
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
        if guard.find_session(session_id).is_some() {
            guard.put_tile_cached(tile_key, webp.clone());
        }
    }

    Ok(webp)
}

// ─────────────────────────── Commands ───────────────────────────

/// `open_large_image` 解码阶段（spawn_blocking 内）的产出。
struct PreparedImage {
    width: u32,
    height: u32,
    preview_rgba: Vec<u8>,
    preview_w: u32,
    preview_h: u32,
    tileable: bool,
    raw_preview: bool,
    tile_source_path: std::path::PathBuf,
    tile_source_is_temp: bool,
}

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
    // CPU 解码线程数：用户设置值 clamp 到 [1, 本机逻辑核]，避免在小核机器上过度订阅。
    let cpu_threads = settings.performance.cpu_threads.clamp(
        1,
        std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(8),
    );
    let system_decode_dir = app
        .path()
        .app_cache_dir()
        .ok()
        .map(|directory| directory.join("system-decode"));
    let large_raster_dir = app
        .path()
        .app_cache_dir()
        .ok()
        .map(|directory| directory.join("large-raster"));

    let path_buf = std::path::PathBuf::from(&path);
    let ext = path_buf
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // spawn_blocking：解码 + 生成预览（非 BMP 大图顺带落临时栅格供分块）
    let path_clone = path_buf.clone();
    #[cfg(debug_assertions)]
    let open_start = std::time::Instant::now();
    let PreparedImage {
        width,
        height,
        preview_rgba,
        preview_w,
        preview_h,
        tileable,
        raw_preview,
        tile_source_path,
        tile_source_is_temp,
    } = tokio::task::spawn_blocking(move || -> Result<PreparedImage, LargeImageError> {
        if ext == "bmp" {
            use crate::large_image::bmp::BmpInfo;
            let info = BmpInfo::from_file(&path_clone)?;
            let (preview, pw, ph) = generate_bmp_preview(&path_clone, preview_max_size, cpu_threads)?;
            return Ok(PreparedImage {
                width: info.width,
                height: info.height,
                preview_rgba: preview,
                preview_w: pw,
                preview_h: ph,
                tileable: true,
                raw_preview: false,
                tile_source_path: path_clone,
                tile_source_is_temp: false,
            });
        }

        if extended_formats::is_system_decoded(&path_clone) {
            let decoded =
                extended_formats::decode_system_image_in(&path_clone, system_decode_dir.as_deref())
                    .map_err(LargeImageError::from_system_decode)?;
            let (w, h) = (decoded.width(), decoded.height());
            let preview_limit = if extended_formats::is_tiff(&path_clone)
                && w as u64 * (h as u64) < settings.large_image.pixel_threshold
            {
                w.max(h)
            } else {
                preview_max_size
            }
            .min(PREVIEW_RAW_CAP);
            let (preview, tw, th) = fast_preview_rgba(&decoded, preview_limit);
            return Ok(PreparedImage {
                width: w,
                height: h,
                preview_rgba: preview,
                preview_w: tw,
                preview_h: th,
                tileable: false,
                raw_preview: extended_formats::is_raw(&path_clone),
                tile_source_path: path_clone,
                tile_source_is_temp: false,
            });
        }

        // 非 BMP 普通格式（PNG/JPEG/WebP…）：解码一次 → 预览 + 临时 32-bit BMP 栅格（供分块）。
        let img = image::open(&path_clone)
            .map_err(|e| LargeImageError::decode(format!("解码图像失败: {e}")))?;
        let (w, h) = (img.width(), img.height());
        // 快速最近邻预览（直接采样源缓冲，比 thumbnail 多遍缩放快得多）。
        let (preview, pw, ph) = fast_preview_rgba(&img, preview_max_size.min(PREVIEW_RAW_CAP));
        let pixels = w as u64 * h as u64;

        // 像素在上限内且有缓存目录：落临时栅格，启用清晰分块。
        if pixels <= MAX_RASTER_PIXELS {
            if let Some(dir) = large_raster_dir.as_deref() {
                let seq = RASTER_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let raster = dir.join(format!("raster-{seq}.bmp"));
                // 无 alpha → 24-bit（省去 RGBA 转换与一份整图大小的缓冲，降低峰值内存）；有 alpha → 32-bit。
                if img.color().has_alpha() {
                    let rgba = img.into_rgba8();
                    write_temp_bmp_raster(rgba.as_raw(), 4, w, h, &raster)?;
                } else {
                    let rgb = img.into_rgb8();
                    write_temp_bmp_raster(rgb.as_raw(), 3, w, h, &raster)?;
                }
                return Ok(PreparedImage {
                    width: w,
                    height: h,
                    preview_rgba: preview,
                    preview_w: pw,
                    preview_h: ph,
                    tileable: true,
                    raw_preview: false,
                    tile_source_path: raster,
                    tile_source_is_temp: true,
                });
            }
        }

        // 像素过大或无缓存目录：仅预览（放大会糊，但内存安全）。
        Ok(PreparedImage {
            width: w,
            height: h,
            preview_rgba: preview,
            preview_w: pw,
            preview_h: ph,
            tileable: false,
            raw_preview: false,
            tile_source_path: path_clone,
            tile_source_is_temp: false,
        })
    })
    .await
    .map_err(|e| LargeImageError::io(format!("spawn_blocking 失败: {e}")))??;

    #[cfg(debug_assertions)]
    println!(
        "[PicSee] open_large_image: {}×{}, preview_gen耗时={}ms",
        width,
        height,
        open_start.elapsed().as_millis()
    );

    // 导航窗小 WebP（从预览 RGBA 降采样后编码，体量小、耗时毫秒级）。
    let nav_preview_webp = make_nav_preview(&preview_rgba, preview_w, preview_h)?;

    // 注册到 managed state
    let state_arc = app.state::<Arc<Mutex<LargeImageState>>>().inner().clone();
    let mut guard = state_arc.lock().unwrap();
    let session_id = guard.next_session_id();
    let session = Arc::new(ImageSession {
        session_id,
        path: path_buf,
        width,
        height,
        tile_size,
        preview_max_size,
        tileable,
        raw_preview,
        tile_source_path,
        tile_source_is_temp,
        preview_rgba,
        nav_preview_webp,
    });
    let result = OpenLargeImageResult {
        session_id: session.session_id,
        width,
        height,
        tile_size,
        preview_max_size,
        tileable,
        raw_preview,
        preview_w,
        preview_h,
    };
    let evicted_temps = guard.add_session(session);
    drop(guard);
    for temp in evicted_temps {
        let _ = std::fs::remove_file(temp);
    }

    Ok(result)
}

/// 关闭大图会话。
#[tauri::command]
pub async fn close_large_image(app: AppHandle, session_id: u64) -> Result<(), LargeImageError> {
    let state_arc = app.state::<Arc<Mutex<LargeImageState>>>().inner().clone();
    let temp = {
        let mut guard = state_arc.lock().unwrap();
        guard.remove_session(session_id)
    };
    if let Some(temp) = temp {
        let _ = std::fs::remove_file(temp);
    }
    Ok(())
}

/// 获取预览原始 RGBA 字节（通过 IPC raw response，避免 fetch 自定义协议在 WKWebView 受限）。
/// 前端按 OpenLargeImageResult.previewW/previewH 构造 ImageData。
#[tauri::command]
pub fn get_preview(app: AppHandle, session_id: u64) -> Result<tauri::ipc::Response, LargeImageError> {
    let state_arc = app.state::<Arc<Mutex<LargeImageState>>>().inner().clone();
    let guard = state_arc.lock().unwrap();
    let session = guard
        .find_session(session_id)
        .ok_or_else(|| LargeImageError::session_not_found(session_id))?;
    Ok(tauri::ipc::Response::new(session.preview_rgba.clone()))
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

    // ── session lifecycle ──

    fn make_state() -> LargeImageState {
        LargeImageState::new(4, 512)
    }

    fn make_session(id: u64) -> Arc<ImageSession> {
        Arc::new(ImageSession {
            session_id: id,
            path: std::path::PathBuf::from("/tmp/test.bmp"),
            width: 1000,
            height: 1000,
            tile_size: 512,
            preview_max_size: 4096,
            tileable: true,
            raw_preview: false,
            tile_source_path: std::path::PathBuf::from("/tmp/test.bmp"),
            tile_source_is_temp: false,
            preview_rgba: vec![],
            nav_preview_webp: vec![],
        })
    }

    #[test]
    fn test_session_lookup() {
        let mut state = make_state();
        state.add_session(make_session(1));
        assert!(state.find_session(1).is_some());
    }

    #[test]
    fn test_session_not_found() {
        let state = make_state();
        assert!(state.find_session(999).is_none());
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

    #[test]
    fn test_session_remove_clears_tile_cache() {
        let mut state = make_state();
        state.add_session(make_session(1));
        state.add_session(make_session(2));
        state.put_tile_cached((1, 0, 0, 0), vec![1, 2, 3]);
        state.put_tile_cached((2, 0, 0, 0), vec![4, 5]);

        state.remove_session(1);

        assert!(state.get_tile_cached((1, 0, 0, 0)).is_none());
        assert_eq!(state.get_tile_cached((2, 0, 0, 0)), Some(vec![4, 5]));
        assert_eq!(state.tile_cache_bytes, 2);
    }

    #[test]
    fn test_session_eviction_clears_tile_cache() {
        let mut state = make_state();
        state.add_session(make_session(1));
        state.put_tile_cached((1, 0, 0, 0), vec![1, 2, 3]);
        state.add_session(make_session(2));
        state.add_session(make_session(3));

        assert!(state.get_tile_cached((1, 0, 0, 0)).is_none());
        assert_eq!(state.tile_cache_bytes, 0);
    }

    #[test]
    fn test_non_tileable_session_returns_explicit_error() {
        let state = Arc::new(Mutex::new(make_state()));
        let mut session = make_session(1);
        Arc::get_mut(&mut session).unwrap().tileable = false;
        state.lock().unwrap().add_session(session);

        let result = handle_tile_request(state, 1, 0, 0, 0);
        let (status, error) = result.unwrap_err();
        assert_eq!(status, 415);
        assert_eq!(error.code, "TILES_UNAVAILABLE");
    }

    #[test]
    fn test_tileable_session_reaches_decoder() {
        let state = Arc::new(Mutex::new(make_state()));
        state.lock().unwrap().add_session(make_session(1));

        let result = handle_tile_request(state, 1, 0, 0, 0);
        let (_, error) = result.unwrap_err();
        assert_eq!(error.code, "IO_ERROR");
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

    #[test]
    fn test_temp_bmp_raster_roundtrip() {
        // 写 RGBA → 临时 32-bit BMP → BmpReader 读回，须逐字节一致（top-down 顺序、RGBA 通道）。
        use crate::large_image::bmp::{BmpReader, Rect};
        let (w, h) = (4u32, 3u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for i in 0..(w * h) as usize {
            rgba[i * 4] = (i * 10) as u8;
            rgba[i * 4 + 1] = (i * 10 + 1) as u8;
            rgba[i * 4 + 2] = (i * 10 + 2) as u8;
            rgba[i * 4 + 3] = 255;
        }
        let f = NamedTempFile::with_suffix(".bmp").unwrap();
        write_temp_bmp_raster(&rgba, 4, w, h, f.path()).unwrap();

        let reader = BmpReader::open(f.path()).unwrap();
        assert_eq!(reader.info.width, w);
        assert_eq!(reader.info.height, h);
        let back = reader
            .read_region(Rect { x: 0, y: 0, width: w, height: h }, w, h)
            .unwrap();
        assert_eq!(back, rgba, "栅格往返 RGBA 必须一致");
    }

    #[test]
    fn test_temp_bmp_raster_roundtrip_24bit() {
        // 写 RGB(3 通道) → 24-bit BMP（行需 4 字节对齐，宽 5 触发填充）→ BmpReader 读回，
        // RGB 一致、alpha 应为 255。
        use crate::large_image::bmp::{BmpReader, Rect};
        let (w, h) = (5u32, 3u32);
        let mut rgb = vec![0u8; (w * h * 3) as usize];
        for i in 0..(w * h) as usize {
            rgb[i * 3] = (i * 9) as u8;
            rgb[i * 3 + 1] = (i * 9 + 1) as u8;
            rgb[i * 3 + 2] = (i * 9 + 2) as u8;
        }
        let f = NamedTempFile::with_suffix(".bmp").unwrap();
        write_temp_bmp_raster(&rgb, 3, w, h, f.path()).unwrap();

        let reader = BmpReader::open(f.path()).unwrap();
        assert_eq!((reader.info.width, reader.info.height), (w, h));
        let back = reader
            .read_region(Rect { x: 0, y: 0, width: w, height: h }, w, h)
            .unwrap();
        for i in 0..(w * h) as usize {
            assert_eq!(&back[i * 4..i * 4 + 3], &rgb[i * 3..i * 3 + 3], "RGB 不一致 @{i}");
            assert_eq!(back[i * 4 + 3], 255, "alpha 应为 255 @{i}");
        }
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
        let (preview, pw, ph) = generate_bmp_preview(f.path(), 4096, 4).unwrap();
        let preview_ms = start.elapsed().as_millis();
        println!(
            "Preview 生成耗时: {preview_ms}ms，{}×{}，大小: {}KB",
            pw,
            ph,
            preview.len() / 1024
        );
        assert!(preview_ms < 3000, "预览生成超时: {preview_ms}ms");
        assert_eq!(preview.len(), pw as usize * ph as usize * 4);

        // 验证瓦片
        let tile = generate_bmp_tile(f.path(), 0, 0, 512, width, height).unwrap();
        assert!(!tile.is_empty());
        println!("Tile (0,0) 大小: {}KB", tile.len() / 1024);
    }

    #[test]
    #[ignore]
    fn integration_workspace_large_bmp_preview_tile() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-assets/test-large.bmp");
        assert!(path.exists(), "缺少集成测试文件: {}", path.display());

        let reader = BmpReader::open(&path).unwrap();
        let preview_start = std::time::Instant::now();
        let (preview, pw, ph) = generate_bmp_preview(&path, 4096, 4).unwrap();
        let preview_ms = preview_start.elapsed().as_millis();

        let tile_start = std::time::Instant::now();
        let tile =
            generate_bmp_tile(&path, 0, 0, 512, reader.info.width, reader.info.height).unwrap();
        let tile_ms = tile_start.elapsed().as_millis();

        assert_eq!(preview.len(), pw as usize * ph as usize * 4);
        assert_eq!(&tile[0..4], b"RIFF");
        println!(
            "Workspace BMP {}×{}: preview={}KB/{}ms, tile={}KB/{}ms",
            reader.info.width,
            reader.info.height,
            preview.len() / 1024,
            preview_ms,
            tile.len() / 1024,
            tile_ms
        );
    }
}
