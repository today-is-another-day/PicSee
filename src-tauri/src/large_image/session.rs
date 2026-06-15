use std::{
    collections::{HashSet, VecDeque},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use lru::LruCache;
use serde::Serialize;
use tauri::{AppHandle, Manager};
use tokio::sync::Semaphore;

use image::ImageDecoder;

use crate::settings::read_settings_file;
use crate::{color, extended_formats};

use super::{
    bmp::{BmpReader, Rect},
    policy::{probe_image_file, LoadMode},
    pyramid::generate_downscaled_raster,
    pyramid_cache::{
        evict_to_limit, load_manifest, pyramid_dir, pyramid_key, touch, write_manifest, LevelMeta,
        PyramidManifest, PYRAMID_ALGO_VERSION,
    },
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

/// 同层构建 claim 的等待间隔与上界，总等待约 2 秒。
const LEVEL_CLAIM_WAIT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);
const LEVEL_CLAIM_WAIT_MAX_POLLS: u32 = 400;

/// 临时栅格文件序号（保证文件名唯一）。
static RASTER_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 单个大图会话。
#[derive(Debug)]
pub struct LevelSource {
    /// 该层像素宽。
    pub width: u32,
    /// 该层像素高。
    pub height: u32,
    /// 该层瓦片读取源。
    pub path: PathBuf,
    /// 是否为本引擎生成、随会话清理的临时栅格。
    pub is_temp: bool,
    /// 该层是否已经完整写入并可读取。
    pub ready: AtomicBool,
}

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
    /// 瓦片读取的源文件：BMP 为原文件，非 BMP 为解码后落盘的临时 BMP 栅格。
    pub tile_source_path: std::path::PathBuf,
    /// tile_source_path 是否为本引擎生成的临时栅格（会话关闭时删除）。
    pub tile_source_is_temp: bool,
    /// 最小的整层可放入单个 tile 的层级。
    pub max_level: u32,
    /// 按 z 索引的层源；level0 初始可用，其余层待后台构建。
    pub levels: Mutex<Vec<Arc<LevelSource>>>,
    /// 会话关闭或逐出后通知后台停止发布新层。
    pub build_cancelled: Arc<AtomicBool>,
    /// 内容寻址持久塔 hash；不可持久化的会话为 None。
    pub pyramid_hash: Option<String>,
    /// 持久塔目录。
    pub pyramid_dir: Option<PathBuf>,
    /// 全局按 hash 去重集合。
    pub global_building: Arc<Mutex<HashSet<String>>>,
    /// 活跃会话 hash 集合，供目录淘汰保护。
    pub protected_hashes: Arc<Mutex<HashSet<String>>>,
    /// 持久塔构建全局 IO 限流。
    pub pyramid_semaphore: Arc<Semaphore>,
    /// 持久塔缓存根目录及配额。
    pub cache_root: Option<PathBuf>,
    pub pyramid_disk_limit_bytes: u64,
    /// 正在构建的层级，避免后台全量构建与按需请求重复写同一层。
    pub building_levels: Arc<Mutex<HashSet<u32>>>,
    /// 待落盘的已解码图（非 BMP 大图）：open 时不写栅格、先返回预览，
    /// 首个瓦片请求或后台任务调用 ensure_raster 时写盘并释放此图。None 表示无需/已完成。
    pub pending_raster: Mutex<Option<image::DynamicImage>>,
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
    /// 持久塔全局单 IO 并发。
    pub pyramid_semaphore: Arc<Semaphore>,
    /// 当前进程正在构建的持久塔 hash。
    pub global_building: Arc<Mutex<HashSet<String>>>,
    pub protected_hashes: Arc<Mutex<HashSet<String>>>,
    pub cache_root: Option<PathBuf>,
    pub pyramid_disk_limit_bytes: u64,
}

impl LargeImageState {
    /// 构造函数。
    ///
    /// - `tile_concurrency`：最大并发解码数。
    /// - `memory_limit_mb`：内存上限（MB），瓦片缓存占其 40%。
    pub fn new(
        tile_concurrency: usize,
        memory_limit_mb: usize,
        cache_root: Option<PathBuf>,
        pyramid_disk_limit_mb: u64,
    ) -> Self {
        let tile_cache_limit_bytes = memory_limit_mb * 1024 * 1024 * 40 / 100;
        Self {
            sessions: VecDeque::new(),
            next_session_id: 1,
            tile_cache: LruCache::new(NonZeroUsize::new(4096).unwrap()),
            tile_cache_bytes: 0,
            tile_cache_limit_bytes,
            semaphore: Arc::new(Semaphore::new(tile_concurrency)),
            pyramid_semaphore: Arc::new(Semaphore::new(1)),
            global_building: Arc::new(Mutex::new(HashSet::new())),
            protected_hashes: Arc::new(Mutex::new(HashSet::new())),
            cache_root,
            pyramid_disk_limit_bytes: pyramid_disk_limit_mb.saturating_mul(1024 * 1024),
        }
    }

    /// 查找会话。
    pub fn find_session(&self, session_id: u64) -> Option<Arc<ImageSession>> {
        self.sessions
            .iter()
            .find(|s| s.session_id == session_id)
            .cloned()
    }

    /// 添加会话；超过 2 个时逐出最旧的。返回被逐出会话的临时栅格路径。
    pub fn add_session(&mut self, session: Arc<ImageSession>) -> Vec<std::path::PathBuf> {
        let mut evicted_temps = Vec::new();
        while self.sessions.len() >= 2 {
            if let Some(evicted) = self.sessions.pop_front() {
                self.clear_session_tiles(evicted.session_id);
                evicted_temps.extend(cancel_and_collect_temp_levels(&evicted));
            }
        }
        self.sessions.push_back(session);
        self.refresh_protected_hashes();
        evicted_temps
    }

    pub fn protected_hashes(&self) -> HashSet<String> {
        let mut protected = self.protected_hashes.lock().unwrap().clone();
        protected.extend(self.global_building.lock().unwrap().iter().cloned());
        protected
    }

    /// 移除指定 session_id 的会话。返回其全部临时栅格路径。
    pub fn remove_session(&mut self, session_id: u64) -> Vec<std::path::PathBuf> {
        let temps = self
            .sessions
            .iter()
            .find(|s| s.session_id == session_id)
            .map(|session| cancel_and_collect_temp_levels(session))
            .unwrap_or_default();
        self.sessions.retain(|s| s.session_id != session_id);
        self.clear_session_tiles(session_id);
        self.refresh_protected_hashes();
        temps
    }

    fn refresh_protected_hashes(&self) {
        *self.protected_hashes.lock().unwrap() = self
            .sessions
            .iter()
            .filter_map(|session| session.pyramid_hash.clone())
            .collect();
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
    pub max_level: u32,
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

/// 计算最小层级，使该层宽高都不超过单个 tile。
pub fn compute_max_level(mut width: u32, mut height: u32, tile_size: u32) -> u32 {
    let mut level = 0;
    while width > tile_size || height > tile_size {
        width = width.div_ceil(2);
        height = height.div_ceil(2);
        level += 1;
    }
    level
}

fn file_fingerprint(
    path: &Path,
    tile_size: u32,
) -> Result<super::pyramid_cache::PyramidKey, LargeImageError> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| LargeImageError::io(format!("读取金字塔源文件元数据失败: {error}")))?;
    let mtime_ns = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos() as i128)
        .unwrap_or(0);
    Ok(pyramid_key(path, metadata.len(), mtime_ns, tile_size))
}

/// 初始化按层源：level0 立即 ready，其余层只发布尺寸和目标路径占位。
fn make_level_sources(
    width: u32,
    height: u32,
    max_level: u32,
    level0_path: PathBuf,
    level0_is_temp: bool,
    pyramid_dir: Option<&Path>,
) -> Vec<Arc<LevelSource>> {
    let mut levels = Vec::with_capacity(max_level as usize + 1);
    let mut level_width = width;
    let mut level_height = height;
    for level in 0..=max_level {
        let (path, is_temp, ready) = if level == 0 {
            (level0_path.clone(), level0_is_temp, true)
        } else {
            (
                pyramid_dir
                    .map(|dir| dir.join(format!("z{level}.bmp")))
                    .unwrap_or_default(),
                pyramid_dir.is_none(),
                false,
            )
        };
        levels.push(Arc::new(LevelSource {
            width: level_width,
            height: level_height,
            path,
            is_temp,
            ready: AtomicBool::new(ready),
        }));
        level_width = level_width.div_ceil(2);
        level_height = level_height.div_ceil(2);
    }
    levels
}

/// 取消后台构建并收集会话创建的临时层文件。
fn cancel_and_collect_temp_levels(session: &ImageSession) -> Vec<PathBuf> {
    session.build_cancelled.store(true, Ordering::Release);
    session
        .levels
        .lock()
        .unwrap()
        .iter()
        .filter(|level| level.is_temp && !level.path.as_os_str().is_empty())
        .map(|level| level.path.clone())
        .collect()
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
        decode_profiled_image(path)?
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

/// 通过 image-rs decoder 提取 ICC，并在解码后转换为 sRGB。
fn decode_profiled_image(path: &Path) -> Result<image::DynamicImage, LargeImageError> {
    let image_decode = (|| -> Result<image::DynamicImage, LargeImageError> {
        let reader = image::ImageReader::open(path)
            .map_err(|e| LargeImageError::decode(format!("打开图像失败: {e}")))?
            .with_guessed_format()
            .map_err(|e| LargeImageError::decode(format!("识别图像格式失败: {e}")))?;
        let mut decoder = reader
            .into_decoder()
            .map_err(|e| LargeImageError::decode(format!("创建图像解码器失败: {e}")))?;
        let icc = decoder.icc_profile().unwrap_or(None);
        let orientation = decoder
            .orientation()
            .unwrap_or(image::metadata::Orientation::NoTransforms);
        let mut img = image::DynamicImage::from_decoder(decoder)
            .map_err(|e| LargeImageError::decode(format!("解码图像失败: {e}")))?;
        if let Some(icc) = icc {
            img = color::dynamic_image_to_srgb(img, &icc);
        }
        img.apply_orientation(orientation);
        Ok(img)
    })();
    match image_decode {
        Ok(img) => Ok(img),
        Err(image_error) => extended_formats::decode_system_image(path).map_err(|_| image_error),
    }
}

// ─────────────────────────── 协议处理器 ───────────────────────────

/// 确保非 BMP 大图的临时栅格已落盘（懒生成）。首次调用写盘并释放解码图；已写过则快速返回。
/// per-session 互斥锁串行化并发的首个瓦片请求与后台任务，保证只写一次。
fn ensure_raster(session: &ImageSession) -> Result<(), LargeImageError> {
    let mut pending = session.pending_raster.lock().unwrap();
    let Some(img) = pending.take() else {
        return Ok(());
    };
    let (w, h) = (session.width, session.height);
    // 无 alpha → 24-bit（更省内存与磁盘）；有 alpha → 32-bit。
    if img.color().has_alpha() {
        let rgba = img.into_rgba8();
        write_temp_bmp_raster(rgba.as_raw(), 4, w, h, &session.tile_source_path)
    } else {
        let rgb = img.into_rgb8();
        write_temp_bmp_raster(rgb.as_raw(), 3, w, h, &session.tile_source_path)
    }
}

/// 层构建 claim；离开作用域或 panic 展开时自动释放。
struct LevelBuildClaim {
    building_levels: Arc<Mutex<HashSet<u32>>>,
    level: u32,
}

/// 持久塔全局构建 claim；离开作用域时释放 hash。
struct PyramidBuildClaim {
    global_building: Arc<Mutex<HashSet<String>>>,
    hash: String,
}

impl Drop for PyramidBuildClaim {
    fn drop(&mut self) {
        self.global_building.lock().unwrap().remove(&self.hash);
    }
}

fn claim_pyramid_build(session: &ImageSession) -> Option<PyramidBuildClaim> {
    let hash = session.pyramid_hash.clone()?;
    if !session.global_building.lock().unwrap().insert(hash.clone()) {
        return None;
    }
    Some(PyramidBuildClaim {
        global_building: session.global_building.clone(),
        hash,
    })
}

fn publish_manifest_levels(session: &ImageSession, manifest: &PyramidManifest) {
    let Some(dir) = session.pyramid_dir.as_deref() else {
        return;
    };
    let mut levels = session.levels.lock().unwrap();
    for meta in &manifest.levels {
        if let Some(level) = levels.get_mut(meta.z as usize) {
            *level = Arc::new(LevelSource {
                width: meta.width,
                height: meta.height,
                path: dir.join(format!("z{}.bmp", meta.z)),
                is_temp: false,
                ready: AtomicBool::new(true),
            });
        }
    }
}

fn build_persistent_pyramid(session: &ImageSession) -> Result<(), LargeImageError> {
    let Some(dir) = session.pyramid_dir.as_deref() else {
        for level in 1..=session.max_level {
            ensure_level(session, level)?;
        }
        return Ok(());
    };
    if let Some(manifest) = load_manifest(dir) {
        publish_manifest_levels(session, &manifest);
        touch(dir);
        return Ok(());
    }
    let Some(_claim) = claim_pyramid_build(session) else {
        return Ok(());
    };
    if let Some(manifest) = load_manifest(dir) {
        publish_manifest_levels(session, &manifest);
        return Ok(());
    }
    for level in 1..=session.max_level {
        if session.build_cancelled.load(Ordering::Acquire) {
            // M3: 已落盘的 z1..zk 是持久层(is_temp=false)，不会被
            // cancel_and_collect_temp_levels 清理；manifest 尚未写成功，故清掉该 hash
            // 目录的半成品，避免长期占配额。正常完成路径（manifest 已写）不会走到这里。
            let _ = std::fs::remove_dir_all(dir);
            return Ok(());
        }
        ensure_level(session, level)?;
    }
    let levels = session.levels.lock().unwrap();
    let manifest = PyramidManifest {
        algo_version: PYRAMID_ALGO_VERSION,
        tile_size: session.tile_size,
        levels: levels
            .iter()
            .enumerate()
            .skip(1)
            .map(|(z, level)| LevelMeta {
                z: z as u32,
                width: level.width,
                height: level.height,
            })
            .collect(),
    };
    drop(levels);
    write_manifest(dir, &manifest)?;
    touch(dir);
    if let Some(root) = session.cache_root.as_deref() {
        let mut protected = session.protected_hashes.lock().unwrap().clone();
        protected.extend(session.global_building.lock().unwrap().iter().cloned());
        evict_to_limit(root, session.pyramid_disk_limit_bytes, &protected);
    }
    Ok(())
}

impl Drop for LevelBuildClaim {
    fn drop(&mut self) {
        self.building_levels.lock().unwrap().remove(&self.level);
    }
}

/// 抢占指定层的构建权。
fn claim_level_build(session: &ImageSession, level: u32) -> Option<LevelBuildClaim> {
    let building_levels = session.building_levels.clone();
    if !building_levels.lock().unwrap().insert(level) {
        return None;
    }
    Some(LevelBuildClaim {
        building_levels,
        level,
    })
}

/// 确保指定层已完整生成；同层并发调用只允许一个实际构建者。
fn ensure_level(session: &ImageSession, level: u32) -> Result<(), LargeImageError> {
    if level > session.max_level {
        return Err(LargeImageError::io(format!(
            "level index out of range: {level} > {}",
            session.max_level
        )));
    }
    for poll in 0..LEVEL_CLAIM_WAIT_MAX_POLLS {
        if session.build_cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        if level > 0 {
            let ready = {
                let levels = session.levels.lock().unwrap();
                levels[level as usize].ready.load(Ordering::Acquire)
            };
            if ready {
                return Ok(());
            }
        }
        if let Some(_claim) = claim_level_build(session, level) {
            return ensure_level_inner(session, level);
        }
        if poll + 1 == LEVEL_CLAIM_WAIT_MAX_POLLS {
            break;
        }
        // 同层由其他任务构建时等待其发布；依赖方向只向更细层，不形成循环等待。
        std::thread::sleep(LEVEL_CLAIM_WAIT_INTERVAL);
    }
    Err(LargeImageError::io(format!(
        "timed out waiting for pyramid level {level} build claim"
    )))
}

/// 已持有当前层构建权时执行构建，依赖层通过 `ensure_level` 单独去重。
fn ensure_level_inner(session: &ImageSession, level: u32) -> Result<(), LargeImageError> {
    if session.build_cancelled.load(Ordering::Acquire) {
        return Ok(());
    }
    if level == 0 {
        ensure_raster(session)?;
        return Ok(());
    }

    let target = {
        let levels = session.levels.lock().unwrap();
        let target = levels
            .get(level as usize)
            .cloned()
            .ok_or_else(|| LargeImageError::tile_out_of_range(0, 0))?;
        if target.ready.load(Ordering::Acquire) {
            return Ok(());
        }
        target
    };
    ensure_level(session, level - 1)?;
    if session.build_cancelled.load(Ordering::Acquire) {
        return Ok(());
    }

    let source = {
        let levels = session.levels.lock().unwrap();
        levels[level as usize - 1].clone()
    };
    if !source.ready.load(Ordering::Acquire) {
        return Err(LargeImageError::io(format!(
            "pyramid dependency level {} is not ready",
            level - 1
        )));
    }
    if target.path.as_os_str().is_empty() {
        return Err(LargeImageError::io("没有可用的金字塔缓存目录"));
    }

    let part_path = target.path.with_extension("bmp.part");
    let generated = generate_downscaled_raster(&source.path, &part_path);
    let (width, height) = match generated {
        Ok(size) => size,
        Err(error) => {
            let _ = std::fs::remove_file(&part_path);
            return Err(error);
        }
    };
    if session.build_cancelled.load(Ordering::Acquire) {
        let _ = std::fs::remove_file(&part_path);
        return Ok(());
    }
    if let Err(error) = std::fs::rename(&part_path, &target.path) {
        let _ = std::fs::remove_file(&part_path);
        return Err(LargeImageError::io(format!("发布金字塔栅格失败: {error}")));
    }

    if session.build_cancelled.load(Ordering::Acquire) {
        return Ok(());
    }
    let ready_level = Arc::new(LevelSource {
        width,
        height,
        path: target.path.clone(),
        is_temp: target.is_temp,
        ready: AtomicBool::new(true),
    });
    session.levels.lock().unwrap()[level as usize] = ready_level;
    Ok(())
}

/// 按需触发层构建；只有成功抢占该层时才创建后台任务。
fn spawn_level_build(session: Arc<ImageSession>, level: u32) {
    if session.build_cancelled.load(Ordering::Acquire) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let Ok(permit) = session.pyramid_semaphore.clone().acquire_owned().await else {
            return;
        };
        let _ = tauri::async_runtime::spawn_blocking(move || {
            let _permit = permit;
            if session.pyramid_hash.is_some() {
                let _ = build_persistent_pyramid(&session);
            } else {
                let _ = ensure_level(&session, level);
            }
        })
        .await;
    });
}

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

    let file = std::fs::File::create(dst)
        .map_err(|e| LargeImageError::io(format!("创建栅格失败: {e}")))?;
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
    z: u32,
    tile_x: u32,
    tile_y: u32,
) -> Result<Vec<u8>, (u16, LargeImageError)> {
    let tile_key: TileKey = (session_id, z, tile_x, tile_y);

    // 先查缓存
    {
        let mut guard = state_arc.lock().unwrap();
        if let Some(cached) = guard.get_tile_cached(tile_key) {
            return Ok(cached);
        }
    }

    // 取出会话（锁外操作）
    let session = {
        let guard = state_arc.lock().unwrap();
        guard.find_session(session_id)
    }
    .ok_or_else(|| (404, LargeImageError::session_not_found(session_id)))?;
    if !session.tileable {
        return Err((415, LargeImageError::tiles_unavailable()));
    }
    if z > session.max_level {
        return Err((400, LargeImageError::tile_out_of_range(tile_x, tile_y)));
    }
    let level = session
        .levels
        .lock()
        .unwrap()
        .get(z as usize)
        .cloned()
        .ok_or_else(|| (400, LargeImageError::tile_out_of_range(tile_x, tile_y)))?;
    if !level.ready.load(Ordering::Acquire) {
        spawn_level_build(session.clone(), z);
        return Err((
            425,
            LargeImageError::new("LEVEL_NOT_READY", format!("Level {z} is not ready")),
        ));
    }
    // 非 BMP level0：首个瓦片触发懒生成栅格；level1 构建也会先执行同一逻辑。
    if z == 0 {
        ensure_raster(&session).map_err(|e| (500u16, e))?;
    }
    let (path, tile_size, img_w, img_h) = (
        level.path.clone(),
        session.tile_size,
        level.width,
        level.height,
    );

    // 验证瓦片范围
    let tiles_x = img_w.div_ceil(tile_size);
    let tiles_y = img_h.div_ceil(tile_size);
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
    /// 非 BMP 大图：待后台/懒生成栅格的已解码图（其余情况为 None）。
    pending_img: Option<image::DynamicImage>,
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
    // CPU 解码线程数：尊重用户设置（设置项已取消上限），仅保留一个防御性硬顶 64，
    // 防止病态线程数；read_region_parallel 内部还会按目标行数再收敛。
    let cpu_threads = settings.performance.cpu_threads.clamp(1, 64);
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
    let cache_root = app.path().app_cache_dir().ok();

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
        pending_img,
    } = tokio::task::spawn_blocking(move || -> Result<PreparedImage, LargeImageError> {
        if ext == "bmp" {
            use crate::large_image::bmp::BmpInfo;
            let info = BmpInfo::from_file(&path_clone)?;
            let (preview, pw, ph) =
                generate_bmp_preview(&path_clone, preview_max_size, cpu_threads)?;
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
                pending_img: None,
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
                pending_img: None,
            });
        }

        // 非 BMP 普通格式（PNG/JPEG/WebP…）：解码一次 → 预览 + 临时 32-bit BMP 栅格（供分块）。
        let img = decode_profiled_image(&path_clone)?;
        let (w, h) = (img.width(), img.height());
        // 快速最近邻预览（直接采样源缓冲，比 thumbnail 多遍缩放快得多）。
        let (preview, pw, ph) = fast_preview_rgba(&img, preview_max_size.min(PREVIEW_RAW_CAP));
        let pixels = w as u64 * h as u64;

        // 像素在上限内且有缓存目录：标记 tileable，但栅格延迟到首个瓦片/后台任务再写
        // （ensure_raster），open 先返回预览，把整图解码移出"打开→出图"关键路径。
        if pixels <= MAX_RASTER_PIXELS {
            if let Some(dir) = large_raster_dir.as_deref() {
                let seq = RASTER_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let raster = dir.join(format!("raster-{seq}.bmp"));
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
                    pending_img: Some(img),
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
            pending_img: None,
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
    let max_level = compute_max_level(width, height, tile_size);
    // M4: fingerprint 失败不应让整个 open 失败（白屏）。失败 → 降级为非持久塔
    // （key=None），仍走临时栅格/level0 正常出 preview。
    let key = match tileable.then(|| file_fingerprint(&path_buf, tile_size)) {
        Some(Ok(key)) => Some(key),
        Some(Err(error)) => {
            eprintln!(
                "[PicSee] 计算金字塔指纹失败，降级为非持久塔: {}",
                error.message
            );
            None
        }
        None => None,
    };
    let persistent_dir = key
        .as_ref()
        .and_then(|key| cache_root.as_deref().map(|root| pyramid_dir(root, key)));
    let manifest = persistent_dir.as_deref().and_then(load_manifest);
    if let Some(dir) = persistent_dir.as_deref() {
        if manifest.is_some() {
            touch(dir);
        }
    }
    let levels = make_level_sources(
        width,
        height,
        max_level,
        tile_source_path.clone(),
        tile_source_is_temp,
        persistent_dir.as_deref(),
    );
    let global_building = guard.global_building.clone();
    let protected_hashes = guard.protected_hashes.clone();
    let pyramid_semaphore = guard.pyramid_semaphore.clone();
    let pyramid_disk_limit_bytes = guard.pyramid_disk_limit_bytes;
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
        max_level,
        levels: Mutex::new(levels),
        build_cancelled: Arc::new(AtomicBool::new(false)),
        pyramid_hash: key.map(|key| key.hash),
        pyramid_dir: persistent_dir,
        global_building,
        protected_hashes,
        pyramid_semaphore,
        cache_root,
        pyramid_disk_limit_bytes,
        building_levels: Arc::new(Mutex::new(HashSet::new())),
        preview_rgba,
        nav_preview_webp,
        pending_raster: Mutex::new(pending_img),
    });
    if let Some(manifest) = manifest.as_ref() {
        publish_manifest_levels(&session, manifest);
    }
    let result = OpenLargeImageResult {
        session_id: session.session_id,
        width,
        height,
        tile_size,
        preview_max_size,
        tileable,
        raw_preview,
        max_level,
        preview_w,
        preview_h,
    };
    // 非 BMP 大图：后台尽快写栅格（即使用户不放大也释放整图内存；首个瓦片请求亦会触发 ensure_raster）。
    // S4: 命中持久 manifest（各层已 ready）时不再 spawn 后台建塔——build_persistent_pyramid
    // 只会 load_manifest 早退、白做一次 IO+publish。仅未命中 manifest（需构建）时才 spawn。
    let bg_pyramid_session = if manifest.is_none()
        && tileable
        && max_level >= 1
        && !session.levels.lock().unwrap()[1]
            .path
            .as_os_str()
            .is_empty()
    {
        Some(session.clone())
    } else {
        None
    };
    let bg_raster_session = if bg_pyramid_session.is_none() && tile_source_is_temp {
        Some(session.clone())
    } else {
        None
    };
    let evicted_temps = guard.add_session(session);
    drop(guard);
    for temp in evicted_temps {
        let _ = std::fs::remove_file(temp);
    }
    if let Some(session) = bg_pyramid_session {
        tauri::async_runtime::spawn(async move {
            let Ok(permit) = session.pyramid_semaphore.clone().acquire_owned().await else {
                return;
            };
            let _ = tauri::async_runtime::spawn_blocking(move || {
                let _permit = permit;
                let _ = build_persistent_pyramid(&session);
            })
            .await;
        });
    } else if let Some(session) = bg_raster_session {
        tauri::async_runtime::spawn_blocking(move || {
            let _ = ensure_raster(&session);
        });
    }

    Ok(result)
}

/// 可测的单路径预建逻辑：仅为可分块的大图 BMP 建持久塔。
fn prefetch_path(
    path: &Path,
    settings: &crate::settings::LargeImageSettings,
    cache_root: &Path,
    global_building: Arc<Mutex<HashSet<String>>>,
    protected_hashes: Arc<Mutex<HashSet<String>>>,
    pyramid_semaphore: Arc<Semaphore>,
    disk_limit_bytes: u64,
) -> Result<bool, LargeImageError> {
    let Ok(probe) = probe_image_file(path, settings) else {
        return Ok(false);
    };
    if !probe.tileable || !probe.is_large || probe.load_mode == LoadMode::Normal {
        return Ok(false);
    }
    let tile_size = settings.tile_size as u32;
    let key = file_fingerprint(path, tile_size)?;
    let dir = pyramid_dir(cache_root, &key);
    if load_manifest(&dir).is_some() {
        touch(&dir);
        return Ok(false);
    }
    let max_level = compute_max_level(probe.width, probe.height, tile_size);
    if max_level == 0 {
        return Ok(false);
    }
    let session = ImageSession {
        session_id: 0,
        path: path.to_path_buf(),
        width: probe.width,
        height: probe.height,
        tile_size,
        preview_max_size: settings.preview_max_size as u32,
        tileable: true,
        raw_preview: false,
        tile_source_path: path.to_path_buf(),
        tile_source_is_temp: false,
        max_level,
        levels: Mutex::new(make_level_sources(
            probe.width,
            probe.height,
            max_level,
            path.to_path_buf(),
            false,
            Some(&dir),
        )),
        build_cancelled: Arc::new(AtomicBool::new(false)),
        pyramid_hash: Some(key.hash),
        pyramid_dir: Some(dir),
        global_building,
        protected_hashes,
        // N1: 复用命令层共享的限流 semaphore（与 open 路径一致），避免误导——
        // ImageSession 内部从不 acquire 该字段，限流实际发生在调用点。
        pyramid_semaphore,
        cache_root: Some(cache_root.to_path_buf()),
        pyramid_disk_limit_bytes: disk_limit_bytes,
        building_levels: Arc::new(Mutex::new(HashSet::new())),
        pending_raster: Mutex::new(None),
        preview_rgba: Vec::new(),
        nav_preview_webp: Vec::new(),
    };
    build_persistent_pyramid(&session)?;
    Ok(true)
}

/// 后台为邻居大图预建持久金字塔；命令本身不等待磁盘任务完成。
#[tauri::command]
pub async fn prefetch_large_pyramid(
    app: AppHandle,
    paths: Vec<String>,
) -> Result<(), LargeImageError> {
    let settings_path = app
        .path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("settings.json"));
    let settings = settings_path
        .as_deref()
        .and_then(|path| read_settings_file(path).ok())
        .unwrap_or_default()
        .large_image;
    let state_arc = app.state::<Arc<Mutex<LargeImageState>>>().inner().clone();
    let (cache_root, global_building, protected_hashes, semaphore, disk_limit_bytes) = {
        let state = state_arc.lock().unwrap();
        (
            state.cache_root.clone(),
            state.global_building.clone(),
            state.protected_hashes.clone(),
            state.pyramid_semaphore.clone(),
            state.pyramid_disk_limit_bytes,
        )
    };
    let Some(cache_root) = cache_root else {
        return Ok(());
    };
    for path in paths {
        let settings = settings.clone();
        let cache_root = cache_root.clone();
        let global_building = global_building.clone();
        let protected_hashes = protected_hashes.clone();
        let semaphore = semaphore.clone();
        tauri::async_runtime::spawn(async move {
            let pyramid_semaphore = semaphore.clone();
            let Ok(permit) = semaphore.acquire_owned().await else {
                return;
            };
            let _ = tauri::async_runtime::spawn_blocking(move || {
                let _permit = permit;
                let _ = prefetch_path(
                    Path::new(&path),
                    &settings,
                    &cache_root,
                    global_building,
                    protected_hashes,
                    pyramid_semaphore,
                    disk_limit_bytes,
                );
            })
            .await;
        });
    }
    Ok(())
}

/// 关闭大图会话。
#[tauri::command]
pub async fn close_large_image(app: AppHandle, session_id: u64) -> Result<(), LargeImageError> {
    let state_arc = app.state::<Arc<Mutex<LargeImageState>>>().inner().clone();
    let temps = {
        let mut guard = state_arc.lock().unwrap();
        guard.remove_session(session_id)
    };
    for temp in temps {
        let _ = std::fs::remove_file(temp);
    }
    Ok(())
}

/// 获取预览原始 RGBA 字节（通过 IPC raw response，避免 fetch 自定义协议在 WKWebView 受限）。
/// 前端按 OpenLargeImageResult.previewW/previewH 构造 ImageData。
#[tauri::command]
pub fn get_preview(
    app: AppHandle,
    session_id: u64,
) -> Result<tauri::ipc::Response, LargeImageError> {
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
    use std::process::Command;
    use tempfile::NamedTempFile;

    fn make_orientation_6_jpeg(width: u32, height: u32) -> Vec<u8> {
        let image = image::DynamicImage::new_rgb8(width, height);
        let mut encoded = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut encoded, image::ImageFormat::Jpeg)
            .unwrap();
        let encoded = encoded.into_inner();

        // JPEG APP1：Exif header + little-endian TIFF，Orientation(0x0112)=6。
        let app1 = [
            0xff, 0xe1, 0x00, 0x22, b'E', b'x', b'i', b'f', 0x00, 0x00, b'I', b'I', 0x2a, 0x00,
            0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x12, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00,
            0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        [&encoded[..2], &app1, &encoded[2..]].concat()
    }

    #[test]
    fn test_decode_profiled_image_applies_exif_orientation() {
        let mut file = NamedTempFile::with_suffix(".jpg").unwrap();
        file.write_all(&make_orientation_6_jpeg(8, 4)).unwrap();

        let decoded = decode_profiled_image(file.path()).unwrap();

        assert_eq!((decoded.width(), decoded.height()), (4, 8));
    }

    #[test]
    fn test_decode_profiled_image_falls_back_to_system_decoder() {
        let directory = tempfile::tempdir_in(".").unwrap();
        let png = directory.path().join("input.png");
        let tiff = directory.path().join("source.tiff");
        let mislabeled = directory.path().join("source.png");
        image::DynamicImage::new_rgb8(8, 6).save(&png).unwrap();
        assert!(Command::new("sips")
            .args(["-s", "format", "tiff"])
            .arg(&png)
            .args(["--out"])
            .arg(&tiff)
            .output()
            .unwrap()
            .status
            .success());
        std::fs::rename(tiff, &mislabeled).unwrap();

        let decoded =
            decode_profiled_image(&mislabeled).expect("image-rs 不支持 TIFF 时应回退系统解码");

        assert_eq!((decoded.width(), decoded.height()), (8, 6));
    }

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

    #[test]
    fn test_compute_max_level_uses_ceil_dimensions() {
        assert_eq!(compute_max_level(19_200, 16_384, 512), 6);
        assert_eq!(compute_max_level(1_000, 1_000, 512), 1);
        assert_eq!(compute_max_level(513, 1, 512), 1);
        assert_eq!(compute_max_level(512, 512, 512), 0);
    }

    #[test]
    fn test_ensure_level_builds_and_publishes_dependency_chain() {
        let directory = tempfile::tempdir_in(".").unwrap();
        let source = directory.path().join("source.bmp");
        let rgba = vec![20u8, 40, 60, 255].repeat(100 * 100);
        write_temp_bmp_raster(&rgba, 4, 100, 100, &source).unwrap();
        let levels = make_level_sources(100, 100, 3, source.clone(), false, Some(directory.path()));
        let session = ImageSession {
            session_id: 7,
            path: source.clone(),
            width: 100,
            height: 100,
            tile_size: 16,
            preview_max_size: 100,
            tileable: true,
            raw_preview: false,
            tile_source_path: source,
            tile_source_is_temp: false,
            max_level: 3,
            levels: Mutex::new(levels),
            build_cancelled: Arc::new(AtomicBool::new(false)),
            pyramid_hash: None,
            pyramid_dir: None,
            global_building: Arc::new(Mutex::new(HashSet::new())),
            protected_hashes: Arc::new(Mutex::new(HashSet::new())),
            pyramid_semaphore: Arc::new(Semaphore::new(1)),
            cache_root: None,
            pyramid_disk_limit_bytes: 0,
            building_levels: Arc::new(Mutex::new(std::collections::HashSet::new())),
            pending_raster: Mutex::new(None),
            preview_rgba: vec![],
            nav_preview_webp: vec![],
        };

        ensure_level(&session, 3).unwrap();

        let levels = session.levels.lock().unwrap();
        for level in &levels[1..=3] {
            assert!(level.ready.load(Ordering::Acquire));
            assert!(level.path.exists());
        }
        assert_eq!((levels[1].width, levels[1].height), (50, 50));
        assert_eq!((levels[2].width, levels[2].height), (25, 25));
        assert_eq!((levels[3].width, levels[3].height), (13, 13));
        assert!(session.building_levels.lock().unwrap().is_empty());
    }

    #[test]
    fn test_concurrent_ensure_level_is_deduplicated() {
        let directory = tempfile::tempdir_in(".").unwrap();
        let source = directory.path().join("source.bmp");
        let rgba = vec![20u8, 40, 60, 255].repeat(100 * 100);
        write_temp_bmp_raster(&rgba, 4, 100, 100, &source).unwrap();
        let session = Arc::new(ImageSession {
            session_id: 8,
            path: source.clone(),
            width: 100,
            height: 100,
            tile_size: 16,
            preview_max_size: 100,
            tileable: true,
            raw_preview: false,
            tile_source_path: source.clone(),
            tile_source_is_temp: false,
            max_level: 3,
            levels: Mutex::new(make_level_sources(
                100,
                100,
                3,
                source,
                false,
                Some(directory.path()),
            )),
            build_cancelled: Arc::new(AtomicBool::new(false)),
            pyramid_hash: None,
            pyramid_dir: None,
            global_building: Arc::new(Mutex::new(HashSet::new())),
            protected_hashes: Arc::new(Mutex::new(HashSet::new())),
            pyramid_semaphore: Arc::new(Semaphore::new(1)),
            cache_root: None,
            pyramid_disk_limit_bytes: 0,
            building_levels: Arc::new(Mutex::new(std::collections::HashSet::new())),
            pending_raster: Mutex::new(None),
            preview_rgba: vec![],
            nav_preview_webp: vec![],
        });

        let left = session.clone();
        let right = session.clone();
        let (left_result, right_result) = std::thread::scope(|scope| {
            let left_task = scope.spawn(|| ensure_level(&left, 2));
            let right_task = scope.spawn(|| ensure_level(&right, 2));
            (left_task.join().unwrap(), right_task.join().unwrap())
        });

        left_result.unwrap();
        right_result.unwrap();
        assert!(session.levels.lock().unwrap()[2]
            .ready
            .load(Ordering::Acquire));
        assert!(session.building_levels.lock().unwrap().is_empty());
    }

    #[test]
    fn test_ensure_level_waits_for_claimed_dependency_then_builds_tail() {
        let directory = tempfile::tempdir_in(".").unwrap();
        let source = directory.path().join("source.bmp");
        let rgba = vec![20u8, 40, 60, 255].repeat(100 * 100);
        write_temp_bmp_raster(&rgba, 4, 100, 100, &source).unwrap();
        let session = Arc::new(ImageSession {
            session_id: 10,
            path: source.clone(),
            width: 100,
            height: 100,
            tile_size: 16,
            preview_max_size: 100,
            tileable: true,
            raw_preview: false,
            tile_source_path: source.clone(),
            tile_source_is_temp: false,
            max_level: 2,
            levels: Mutex::new(make_level_sources(
                100,
                100,
                2,
                source,
                false,
                Some(directory.path()),
            )),
            build_cancelled: Arc::new(AtomicBool::new(false)),
            pyramid_hash: None,
            pyramid_dir: None,
            global_building: Arc::new(Mutex::new(HashSet::new())),
            protected_hashes: Arc::new(Mutex::new(HashSet::new())),
            pyramid_semaphore: Arc::new(Semaphore::new(1)),
            cache_root: None,
            pyramid_disk_limit_bytes: 0,
            building_levels: Arc::new(Mutex::new(HashSet::new())),
            pending_raster: Mutex::new(None),
            preview_rgba: vec![],
            nav_preview_webp: vec![],
        });
        let dependency_claim = claim_level_build(&session, 1).expect("应成功抢占依赖层");

        let worker = session.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let task = std::thread::spawn(move || {
            sender.send(ensure_level(&worker, 2)).unwrap();
        });

        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(receiver.try_recv().is_err());
        drop(dependency_claim);
        receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap();
        task.join().unwrap();

        let levels = session.levels.lock().unwrap();
        assert!(levels[1].ready.load(Ordering::Acquire));
        assert!(levels[2].ready.load(Ordering::Acquire));
    }

    #[test]
    fn test_level_build_claim_is_released_after_panic() {
        let session = make_session(13);

        let result = std::panic::catch_unwind(|| {
            let _claim = claim_level_build(&session, 1).expect("应成功抢占测试层");
            panic!("模拟构建线程 panic");
        });

        assert!(result.is_err());
        assert!(session.building_levels.lock().unwrap().is_empty());
    }

    #[test]
    fn test_pyramid_build_claim_deduplicates_across_sessions_and_releases() {
        let global = Arc::new(Mutex::new(HashSet::new()));
        let mut first = make_session(21);
        let mut second = make_session(22);
        Arc::get_mut(&mut first).unwrap().pyramid_hash = Some("same".to_string());
        Arc::get_mut(&mut first).unwrap().global_building = global.clone();
        Arc::get_mut(&mut second).unwrap().pyramid_hash = Some("same".to_string());
        Arc::get_mut(&mut second).unwrap().global_building = global.clone();

        let claim = claim_pyramid_build(&first).expect("首个 session 应获得全局 claim");
        assert!(claim_pyramid_build(&second).is_none());
        drop(claim);
        assert!(claim_pyramid_build(&second).is_some());
    }

    #[test]
    fn test_ensure_level_claim_wait_is_bounded() {
        let session = make_session(14);
        let claim = claim_level_build(&session, 1).expect("应成功抢占测试层");
        let started = std::time::Instant::now();

        let error = ensure_level(&session, 1).unwrap_err();

        assert_eq!(error.code, "IO_ERROR");
        assert!(error.message.contains("timed out"));
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
        drop(claim);
    }

    #[test]
    fn test_spawn_level_build_skips_cancelled_session() {
        let session = make_session(11);
        session.build_cancelled.store(true, Ordering::Release);

        spawn_level_build(session.clone(), 1);

        assert!(session.building_levels.lock().unwrap().is_empty());
    }

    #[test]
    fn test_ensure_level_above_max_reports_internal_range_error() {
        let session = make_session(12);

        let error = ensure_level(&session, session.max_level + 1).unwrap_err();

        assert_eq!(error.code, "IO_ERROR");
        assert!(error.message.contains("level index out of range"));
    }

    // ── session lifecycle ──

    fn make_state() -> LargeImageState {
        LargeImageState::new(4, 512, None, 0)
    }

    fn make_session(id: u64) -> Arc<ImageSession> {
        let tile_source_path = std::path::PathBuf::from("target/test.bmp");
        let max_level = compute_max_level(1000, 1000, 512);
        Arc::new(ImageSession {
            session_id: id,
            path: tile_source_path.clone(),
            width: 1000,
            height: 1000,
            tile_size: 512,
            preview_max_size: 4096,
            tileable: true,
            raw_preview: false,
            tile_source_path: tile_source_path.clone(),
            tile_source_is_temp: false,
            max_level,
            levels: Mutex::new(make_level_sources(
                1000,
                1000,
                max_level,
                tile_source_path,
                false,
                None,
            )),
            build_cancelled: Arc::new(AtomicBool::new(false)),
            pyramid_hash: None,
            pyramid_dir: None,
            global_building: Arc::new(Mutex::new(HashSet::new())),
            protected_hashes: Arc::new(Mutex::new(HashSet::new())),
            pyramid_semaphore: Arc::new(Semaphore::new(1)),
            cache_root: None,
            pyramid_disk_limit_bytes: 0,
            building_levels: Arc::new(Mutex::new(std::collections::HashSet::new())),
            preview_rgba: vec![],
            nav_preview_webp: vec![],
            pending_raster: Mutex::new(None),
        })
    }

    #[test]
    fn test_session_lookup() {
        let mut state = make_state();
        state.add_session(make_session(1));
        let session = state.find_session(1).unwrap();
        assert!(session.levels.lock().unwrap()[0]
            .ready
            .load(std::sync::atomic::Ordering::Acquire));
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
    fn test_persistent_levels_survive_session_removal() {
        let directory = tempfile::tempdir_in(".").unwrap();
        let persistent = directory.path().join("z1.bmp");
        std::fs::write(&persistent, b"persistent").unwrap();
        let mut session = make_session(30);
        let session_mut = Arc::get_mut(&mut session).unwrap();
        session_mut.pyramid_hash = Some("persistent".to_string());
        session_mut.levels.lock().unwrap()[1] = Arc::new(LevelSource {
            width: 500,
            height: 500,
            path: persistent.clone(),
            is_temp: false,
            ready: AtomicBool::new(true),
        });
        let mut state = make_state();
        state.add_session(session);

        for temp in state.remove_session(30) {
            let _ = std::fs::remove_file(temp);
        }

        assert!(persistent.exists());
    }

    #[test]
    fn test_prefetch_path_builds_manifest_and_reuses_it() {
        let directory = tempfile::tempdir_in(".").unwrap();
        let source = directory.path().join("source.bmp");
        let rgba = vec![20u8, 40, 60, 255].repeat(600 * 600);
        write_temp_bmp_raster(&rgba, 4, 600, 600, &source).unwrap();
        let mut settings = crate::settings::LargeImageSettings::default();
        settings.pixel_threshold = 1;
        let global = Arc::new(Mutex::new(HashSet::new()));
        let protected = Arc::new(Mutex::new(HashSet::new()));

        assert!(prefetch_path(
            &source,
            &settings,
            directory.path(),
            global.clone(),
            protected.clone(),
            Arc::new(Semaphore::new(1)),
            u64::MAX,
        )
        .unwrap());
        let dir = pyramid_dir(
            directory.path(),
            &file_fingerprint(&source, settings.tile_size as u32).unwrap(),
        );
        assert!(load_manifest(&dir).is_some());
        let modified = std::fs::metadata(dir.join("z1.bmp"))
            .unwrap()
            .modified()
            .unwrap();

        assert!(!prefetch_path(
            &source,
            &settings,
            directory.path(),
            global,
            protected,
            Arc::new(Semaphore::new(1)),
            u64::MAX,
        )
        .unwrap());
        assert_eq!(
            std::fs::metadata(dir.join("z1.bmp"))
                .unwrap()
                .modified()
                .unwrap(),
            modified
        );
    }

    #[test]
    fn test_prefetch_path_safely_skips_invalid_and_small_files() {
        let directory = tempfile::tempdir_in(".").unwrap();
        let invalid = directory.path().join("invalid.txt");
        std::fs::write(&invalid, b"not image").unwrap();
        let small = directory.path().join("small.bmp");
        let rgba = vec![20u8, 40, 60, 255].repeat(8 * 8);
        write_temp_bmp_raster(&rgba, 4, 8, 8, &small).unwrap();
        let global = Arc::new(Mutex::new(HashSet::new()));
        let protected = Arc::new(Mutex::new(HashSet::new()));
        let settings = crate::settings::LargeImageSettings::default();

        assert!(!prefetch_path(
            &invalid,
            &settings,
            directory.path(),
            global.clone(),
            protected.clone(),
            Arc::new(Semaphore::new(1)),
            u64::MAX,
        )
        .unwrap());
        assert!(!prefetch_path(
            &small,
            &settings,
            directory.path(),
            global,
            protected,
            Arc::new(Semaphore::new(1)),
            u64::MAX,
        )
        .unwrap());
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

    #[test]
    fn test_level_not_ready_is_retryable() {
        let state = Arc::new(Mutex::new(make_state()));
        state.lock().unwrap().add_session(make_session(1));

        let (status, error) = handle_tile_request(state, 1, 1, 0, 0).unwrap_err();
        assert_eq!(status, 425);
        assert_eq!(error.code, "LEVEL_NOT_READY");
    }

    #[test]
    fn test_level_not_ready_triggers_on_demand_build() {
        let directory = tempfile::tempdir_in(".").unwrap();
        let source = directory.path().join("source.bmp");
        let rgba = vec![20u8, 40, 60, 255].repeat(100 * 100);
        write_temp_bmp_raster(&rgba, 4, 100, 100, &source).unwrap();
        let session = Arc::new(ImageSession {
            session_id: 9,
            path: source.clone(),
            width: 100,
            height: 100,
            tile_size: 16,
            preview_max_size: 100,
            tileable: true,
            raw_preview: false,
            tile_source_path: source.clone(),
            tile_source_is_temp: false,
            max_level: 3,
            levels: Mutex::new(make_level_sources(
                100,
                100,
                3,
                source,
                false,
                Some(directory.path()),
            )),
            build_cancelled: Arc::new(AtomicBool::new(false)),
            pyramid_hash: None,
            pyramid_dir: None,
            global_building: Arc::new(Mutex::new(HashSet::new())),
            protected_hashes: Arc::new(Mutex::new(HashSet::new())),
            pyramid_semaphore: Arc::new(Semaphore::new(1)),
            cache_root: None,
            pyramid_disk_limit_bytes: 0,
            building_levels: Arc::new(Mutex::new(HashSet::new())),
            pending_raster: Mutex::new(None),
            preview_rgba: vec![],
            nav_preview_webp: vec![],
        });
        let state = Arc::new(Mutex::new(make_state()));
        state.lock().unwrap().add_session(session.clone());

        let (status, error) = handle_tile_request(state, 9, 3, 0, 0).unwrap_err();
        assert_eq!(status, 425);
        assert_eq!(error.code, "LEVEL_NOT_READY");
        for _ in 0..100 {
            if session.levels.lock().unwrap()[3]
                .ready
                .load(Ordering::Acquire)
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("按需构建未在超时前发布 level3");
    }

    #[test]
    fn test_level_above_max_is_out_of_range() {
        let state = Arc::new(Mutex::new(make_state()));
        state.lock().unwrap().add_session(make_session(1));

        let (status, error) = handle_tile_request(state, 1, 2, 0, 0).unwrap_err();
        assert_eq!(status, 400);
        assert_eq!(error.code, "TILE_OUT_OF_RANGE");
    }

    #[test]
    fn test_per_level_tile_grid_is_validated() {
        let state = Arc::new(Mutex::new(make_state()));
        let session = make_session(1);
        session.levels.lock().unwrap()[1]
            .ready
            .store(true, Ordering::Release);
        state.lock().unwrap().add_session(session);

        let (status, error) = handle_tile_request(state, 1, 1, 1, 0).unwrap_err();
        assert_eq!(status, 400);
        assert_eq!(error.code, "TILE_OUT_OF_RANGE");
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
        let mut state = LargeImageState::new(4, 1, None, 0);
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
            .read_region(
                Rect {
                    x: 0,
                    y: 0,
                    width: w,
                    height: h,
                },
                w,
                h,
            )
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
            .read_region(
                Rect {
                    x: 0,
                    y: 0,
                    width: w,
                    height: h,
                },
                w,
                h,
            )
            .unwrap();
        for i in 0..(w * h) as usize {
            assert_eq!(
                &back[i * 4..i * 4 + 3],
                &rgb[i * 3..i * 3 + 3],
                "RGB 不一致 @{i}"
            );
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
