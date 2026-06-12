use image::{DynamicImage, GenericImageView, ImageFormat};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tauri::{AppHandle, Manager};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// 支持缩略图生成的格式扩展名（小写）。
const THUMBNAIL_EXTENSIONS: [&str; 6] = ["jpg", "jpeg", "png", "webp", "gif", "bmp"];

/// 大图跳过阈值：单边超过此值或文件超过 MAX_FILE_BYTES 时跳过缩略图。
const MAX_SIDE_PIXELS: u32 = 12_000;
const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024; // 100 MB

/// 结构化错误，便于前端按 code 映射 i18n。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailError {
    /// 错误码，前端据此选择 i18n 文案。
    pub code: &'static str,
    /// 补充说明（英文），code 未知时前端可回退显示此字段。
    pub message: String,
}

impl ThumbnailError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

/// in-flight 任务结果类型。
type InFlightResult = Option<Result<PathBuf, String>>;
/// in-flight watch sender 类型。
type InFlightSender = Arc<tokio::sync::watch::Sender<InFlightResult>>;

/// 并发控制状态，通过 Tauri managed state 共享。
pub struct ThumbnailState {
    semaphore: Arc<Semaphore>,
    /// in-flight map：cache_key → watch sender，用于合并同一文件的并发请求。
    in_flight: Mutex<HashMap<String, InFlightSender>>,
}

impl ThumbnailState {
    pub fn new(concurrency: u32) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(concurrency as usize)),
            in_flight: Mutex::new(HashMap::new()),
        }
    }
}

/// 获取缩略图命令。
/// 返回磁盘缓存文件的绝对路径，前端用 convertFileSrc 显示。
/// SVG 文件不经此命令，前端直接用原文件。
#[tauri::command]
pub async fn get_thumbnail(
    app: AppHandle,
    path: String,
    size: u32,
) -> Result<String, ThumbnailError> {
    // 限制 size 只允许合法值
    let size = match size {
        96 | 160 | 256 => size,
        _ => 160,
    };

    let file_path = PathBuf::from(&path);

    // 检查扩展名，SVG 不走此命令
    let ext = file_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "svg" {
        return Err(ThumbnailError::new("UNSUPPORTED_FORMAT", "SVG 文件应由前端直接显示"));
    }
    if !THUMBNAIL_EXTENSIONS.contains(&ext.as_str()) {
        return Err(ThumbnailError::new("UNSUPPORTED_FORMAT", format!("不支持的格式: {ext}")));
    }

    // 检查路径是否在已授权 asset scope 内
    if !app.asset_protocol_scope().is_allowed(&file_path) {
        return Err(ThumbnailError::new("NOT_ALLOWED", format!("路径未经授权: {path}")));
    }

    // 读取文件元数据以计算 cache key
    let metadata = fs::metadata(&file_path)
        .map_err(|e| ThumbnailError::new("IO_ERROR", format!("读取文件元数据失败: {e}")))?;

    let file_size = metadata.len();
    if file_size > MAX_FILE_BYTES {
        return Err(ThumbnailError::new("FILE_TOO_LARGE", "文件超过 100MB，跳过缩略图"));
    }

    let modified = metadata
        .modified()
        .map(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        })
        .unwrap_or(0);

    // 计算稳定 cache key
    let canonical = fs::canonicalize(&file_path).unwrap_or_else(|_| file_path.clone());
    let cache_key = compute_cache_key(&canonical, file_size, modified, size);

    // 缓存目录
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| ThumbnailError::new("IO_ERROR", format!("获取缓存目录失败: {e}")))?
        .join("thumbnails");
    let cache_file = cache_dir.join(format!("{cache_key}.webp"));

    // 命中磁盘缓存时直接授权并返回
    if cache_file.exists() {
        ensure_cache_scope(&app, &cache_dir)?;
        return Ok(cache_file.to_string_lossy().into_owned());
    }

    // in-flight 合并：同一 key 的并发请求只生成一次
    let state = app.state::<ThumbnailState>();
    let maybe_rx: Option<tokio::sync::watch::Receiver<InFlightResult>> = {
        let mut map = state.in_flight.lock().unwrap();
        if let Some(tx) = map.get(&cache_key) {
            // 已有正在进行的生成任务，等待其结果
            Some(tx.subscribe())
        } else {
            // 注册占位
            let (tx, _rx) =
                tokio::sync::watch::channel::<InFlightResult>(None);
            map.insert(cache_key.clone(), Arc::new(tx));
            None
        }
    };

    if let Some(mut rx) = maybe_rx {
        // 等待已有任务完成
        rx.changed()
            .await
            .map_err(|_| ThumbnailError::new("IO_ERROR", "等待缩略图生成任务时通道关闭"))?;
        let result: InFlightResult = rx.borrow().clone();
        return match result {
            Some(Ok(out_path)) => Ok(out_path.to_string_lossy().into_owned()),
            Some(Err(e)) => Err(ThumbnailError::new("DECODE_ERROR", e)),
            None => Err(ThumbnailError::new("IO_ERROR", "缩略图生成任务未产生结果")),
        };
    }

    // 获取并发信号量
    let permit: OwnedSemaphorePermit = Arc::clone(&state.semaphore)
        .acquire_owned()
        .await
        .map_err(|_| ThumbnailError::new("IO_ERROR", "信号量已关闭"))?;

    // 在 blocking 线程中生成缩略图
    let cache_dir_clone = cache_dir.clone();
    let cache_file_clone = cache_file.clone();
    let cache_key_clone = cache_key.clone();
    let path_clone = path.clone();

    let result: Result<PathBuf, String> = tauri::async_runtime::spawn_blocking(move || {
        // permit 在此闭包结束时释放
        let _permit: OwnedSemaphorePermit = permit;
        generate_thumbnail(&path_clone, &cache_dir_clone, &cache_file_clone, size)
    })
    .await
    .map_err(|e| format!("生成缩略图任务崩溃: {e}"))
    .and_then(|r| r);

    // 通知等待者
    {
        let mut map = state.in_flight.lock().unwrap();
        if let Some(tx) = map.remove(&cache_key_clone) {
            let notify_value: InFlightResult = match &result {
                Ok(p) => Some(Ok(p.clone())),
                Err(e) => Some(Err(e.clone())),
            };
            let _ = tx.send(notify_value);
        }
    }

    match result {
        Ok(out_path) => {
            ensure_cache_scope(&app, &cache_dir)?;
            Ok(out_path.to_string_lossy().into_owned())
        }
        Err(e) => Err(ThumbnailError::new("DECODE_ERROR", e)),
    }
}

/// 清理缩略图磁盘缓存，返回释放的字节数。
#[tauri::command]
pub async fn clear_thumbnail_cache(app: AppHandle) -> Result<u64, ThumbnailError> {
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| ThumbnailError::new("IO_ERROR", format!("获取缓存目录失败: {e}")))?
        .join("thumbnails");

    if !cache_dir.exists() {
        return Ok(0);
    }

    let freed = tauri::async_runtime::spawn_blocking(move || {
        let mut total: u64 = 0;
        let entries = fs::read_dir(&cache_dir)
            .map_err(|e| format!("读取缓存目录失败: {e}"))?;
        for entry in entries.filter_map(Result::ok) {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    total += meta.len();
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        Ok::<u64, String>(total)
    })
    .await
    .map_err(|e| ThumbnailError::new("IO_ERROR", format!("清理任务崩溃: {e}")))?
    .map_err(|e| ThumbnailError::new("IO_ERROR", e))?;

    Ok(freed)
}

// ──────────────────────────────────────────────────────────────────────────────
// 内部辅助函数（pub 供测试模块使用）
// ──────────────────────────────────────────────────────────────────────────────

/// 计算稳定 cache key（SHA-256 前 16 字节十六进制，共 32 字符）。
pub fn compute_cache_key(
    canonical_path: &Path,
    file_size: u64,
    modified_ms: u128,
    size: u32,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(canonical_path.to_string_lossy().as_bytes());
    hasher.update(b":");
    hasher.update(file_size.to_le_bytes());
    hasher.update(b":");
    hasher.update(modified_ms.to_le_bytes());
    hasher.update(b":");
    hasher.update(size.to_le_bytes());
    let digest = hasher.finalize();
    // 取前 16 字节（128 位）→ 32 字符 hex
    let bytes: &[u8] = &digest[..16];
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 生成缩略图并写入磁盘，返回缓存文件路径。
pub fn generate_thumbnail(
    src_path: &str,
    cache_dir: &Path,
    cache_file: &Path,
    size: u32,
) -> Result<PathBuf, String> {
    // 确保缓存目录存在
    fs::create_dir_all(cache_dir)
        .map_err(|e| format!("创建缓存目录失败: {e}"))?;

    let path = Path::new(src_path);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    // 读取原始字节
    let raw = fs::read(path)
        .map_err(|e| format!("读取图片文件失败: {e}"))?;

    // 解码图片
    let img = decode_image(&raw, &ext, path)
        .map_err(|e| format!("解码图片失败: {e}"))?;

    // 检查单边像素上限（BMP 等可能很大）
    let (w, h) = img.dimensions();
    if w > MAX_SIDE_PIXELS || h > MAX_SIDE_PIXELS {
        return Err(format!("图片单边超过 {MAX_SIDE_PIXELS} 像素，跳过缩略图"));
    }

    // 按比例缩小到 size×size 内
    let thumb = img.thumbnail(size, size);

    // 编码为 WebP（image 0.25 内置支持）
    let mut buf = Cursor::new(Vec::new());
    thumb
        .write_to(&mut buf, ImageFormat::WebP)
        .map_err(|e| format!("编码 WebP 失败: {e}"))?;

    // 原子写入（先写临时文件再 rename）
    let tmp = cache_file.with_extension("tmp");
    fs::write(&tmp, buf.into_inner())
        .map_err(|e| format!("写入临时缓存文件失败: {e}"))?;
    fs::rename(&tmp, cache_file).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("替换缓存文件失败: {e}")
    })?;

    Ok(cache_file.to_path_buf())
}

/// 解码图片，JPG/JPEG 时先读取并应用 EXIF orientation。
pub fn decode_image(raw: &[u8], ext: &str, path: &Path) -> Result<DynamicImage, String> {
    // GIF 只取首帧（image crate load_from_memory 默认取第一帧）
    if ext == "gif" {
        let img = image::load_from_memory_with_format(raw, ImageFormat::Gif)
            .map_err(|e| format!("解码 GIF 失败: {e}"))?;
        return Ok(img);
    }

    let img = image::load_from_memory(raw)
        .map_err(|e| format!("解码图片失败 ({}): {e}", path.display()))?;

    // JPEG 应用 EXIF orientation
    if matches!(ext, "jpg" | "jpeg") {
        let oriented = apply_exif_orientation(img, raw);
        return Ok(oriented);
    }

    Ok(img)
}

/// 读取 EXIF Orientation 并对图片进行旋转/翻转（对应 EXIF orientation 1-8）。
pub fn apply_exif_orientation(img: DynamicImage, raw: &[u8]) -> DynamicImage {
    let orientation = read_exif_orientation(raw).unwrap_or(1);
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img, // 1 或未知：不旋转
    }
}

/// 从原始 JPEG 字节读取 EXIF Orientation 值（1-8）。
pub fn read_exif_orientation(raw: &[u8]) -> Option<u32> {
    let exif_reader = exif::Reader::new();
    let mut cursor = std::io::Cursor::new(raw);
    let exif = exif_reader.read_from_container(&mut cursor).ok()?;
    let field = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)?;
    match &field.value {
        exif::Value::Short(values) => values.first().map(|v| *v as u32),
        _ => None,
    }
}

/// 将缓存目录授权到 asset protocol scope（幂等）。
fn ensure_cache_scope(app: &AppHandle, cache_dir: &Path) -> Result<(), ThumbnailError> {
    app.asset_protocol_scope()
        .allow_directory(cache_dir, false)
        .map_err(|e| ThumbnailError::new("IO_ERROR", format!("授权缓存目录失败: {e}")))
}
