use crate::{color, extended_formats};
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};
use tauri::{AppHandle, Manager};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::settings::read_settings_file;

/// Supported thumbnail format extensions (lowercase).
const THUMBNAIL_EXTENSIONS: [&str; 18] = [
    "jpg", "jpeg", "png", "webp", "gif", "bmp", "tiff", "tif", "heic", "heif", "dng", "cr2", "cr3",
    "nef", "arw", "raf", "orf", "rw2",
];

/// 超过此单边像素或 MAX_INPROC_PIXELS 时改用 sips 子进程降采样（内存隔离），不在本进程整图解码。
const MAX_SIDE_PIXELS: u32 = 12_000;
/// 本进程整图解码的像素上限；超过改走 sips（避免大图缩略图占用过多内存）。
const MAX_INPROC_PIXELS: u64 = 40_000_000;
/// 文件大小硬上限（防御病态超大文件；常规大图由 sips 降采样处理）。
const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024; // 1 GB
/// 长会话每新增约 64MB 缩略图后触发一次后台容量检查，避免每次写盘都扫描目录。
const DISK_CACHE_SWEEP_WRITE_THRESHOLD_BYTES: u64 = 64 * 1024 * 1024;

/// Structured error for frontend i18n mapping by code.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailError {
    /// Error code; frontend selects the i18n string based on this.
    pub code: &'static str,
    /// Supplementary message (English); shown as fallback when code is unknown.
    pub message: String,
}

impl ThumbnailError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Internal error enum for generate_thumbnail, preserving semantic error codes.
#[derive(Debug)]
pub enum GenError {
    ImageTooLarge,
    DecodeFailed(String),
    IoFailed(String),
}

impl GenError {
    fn into_thumbnail_error(self) -> ThumbnailError {
        match self {
            GenError::ImageTooLarge => ThumbnailError::new(
                "IMAGE_TOO_LARGE",
                format!("Image side exceeds {MAX_SIDE_PIXELS} pixels; thumbnail skipped"),
            ),
            GenError::DecodeFailed(msg) => ThumbnailError::new("DECODE_ERROR", msg),
            GenError::IoFailed(msg) => ThumbnailError::new("IO_ERROR", msg),
        }
    }
}

/// In-flight task result type — carries semantic error code as string.
type InFlightResult = Option<Result<PathBuf, (/* code */ &'static str, String)>>;
/// In-flight watch sender type.
type InFlightSender = Arc<tokio::sync::watch::Sender<InFlightResult>>;

/// Concurrency control state, shared via Tauri managed state.
pub struct ThumbnailState {
    semaphore: Arc<Semaphore>,
    /// in-flight map: cache_key → watch sender, merges concurrent requests for the same file.
    in_flight: Mutex<HashMap<String, InFlightSender>>,
    /// 上次容量检查后新增的缩略图字节数。
    bytes_since_disk_cache_sweep: AtomicU64,
}

impl ThumbnailState {
    pub fn new(concurrency: u32) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(concurrency as usize)),
            in_flight: Mutex::new(HashMap::new()),
            bytes_since_disk_cache_sweep: AtomicU64::new(0),
        }
    }

    /// 累计本次写盘大小；跨过阈值时仅允许一个调用方触发后台扫描。
    fn record_disk_cache_write(&self, bytes: u64) -> bool {
        let total = self
            .bytes_since_disk_cache_sweep
            .fetch_add(bytes, Ordering::Relaxed)
            .saturating_add(bytes);
        if total < DISK_CACHE_SWEEP_WRITE_THRESHOLD_BYTES {
            return false;
        }

        self.bytes_since_disk_cache_sweep
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |current| {
                (current >= DISK_CACHE_SWEEP_WRITE_THRESHOLD_BYTES).then_some(0)
            })
            .is_ok()
    }
}

/// Get thumbnail command.
/// Returns the absolute path of the disk-cached thumbnail; frontend uses convertFileSrc to display it.
/// SVG files are not handled here — the frontend displays them directly.
#[tauri::command]
pub async fn get_thumbnail(
    app: AppHandle,
    path: String,
    size: u32,
) -> Result<String, ThumbnailError> {
    // Restrict size to valid values.
    let size = match size {
        96 | 160 | 256 => size,
        _ => 160,
    };

    let file_path = PathBuf::from(&path);

    // Check extension; SVG must not reach this command.
    let ext = file_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "svg" {
        return Err(ThumbnailError::new(
            "UNSUPPORTED_FORMAT",
            "SVG files should be displayed directly by the frontend",
        ));
    }
    if !THUMBNAIL_EXTENSIONS.contains(&ext.as_str()) {
        return Err(ThumbnailError::new(
            "UNSUPPORTED_FORMAT",
            format!("Unsupported format: {ext}"),
        ));
    }

    // Minor 9: use canonical path for NOT_ALLOWED check to avoid symlink bypass.
    let canonical = fs::canonicalize(&file_path).unwrap_or_else(|_| file_path.clone());
    if !app.asset_protocol_scope().is_allowed(&canonical) {
        return Err(ThumbnailError::new(
            "NOT_ALLOWED",
            format!("Path not authorized: {path}"),
        ));
    }

    // Read file metadata to compute cache key.
    let metadata = fs::metadata(&file_path).map_err(|e| {
        ThumbnailError::new("IO_ERROR", format!("Failed to read file metadata: {e}"))
    })?;

    let file_size = metadata.len();
    if file_size > MAX_FILE_BYTES {
        return Err(ThumbnailError::new(
            "FILE_TOO_LARGE",
            "File exceeds 100 MB; thumbnail skipped",
        ));
    }

    let modified = metadata
        .modified()
        .map(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        })
        .unwrap_or(0);

    // Compute stable cache key.
    let cache_key = compute_cache_key(&canonical, file_size, modified, size);

    // Cache directory.
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| {
            ThumbnailError::new("IO_ERROR", format!("Failed to get cache directory: {e}"))
        })?
        .join("thumbnails");
    let cache_file = cache_dir.join(format!("{cache_key}.webp"));

    // Minor 2: grant asset scope once at setup; here just ensure the dir exists in scope on cache hit.
    if cache_file.exists() {
        ensure_cache_scope(&app, &cache_dir)?;
        return Ok(cache_file.to_string_lossy().into_owned());
    }

    // In-flight deduplication: only one generation task per cache key.
    let state = app.state::<ThumbnailState>();
    let maybe_rx: Option<tokio::sync::watch::Receiver<InFlightResult>> = {
        let mut map = state.in_flight.lock().unwrap();
        if let Some(tx) = map.get(&cache_key) {
            // A generation task is already running; subscribe to its result.
            Some(tx.subscribe())
        } else {
            // Register placeholder.
            let (tx, _rx) = tokio::sync::watch::channel::<InFlightResult>(None);
            map.insert(cache_key.clone(), Arc::new(tx));
            None
        }
    };

    if let Some(mut rx) = maybe_rx {
        // Wait for the existing task to complete.
        rx.changed().await.map_err(|_| {
            ThumbnailError::new(
                "IO_ERROR",
                "Channel closed while waiting for thumbnail task",
            )
        })?;
        let result: InFlightResult = rx.borrow().clone();
        return match result {
            Some(Ok(out_path)) => {
                ensure_cache_scope(&app, &cache_dir)?;
                Ok(out_path.to_string_lossy().into_owned())
            }
            // M2: forward the original error code faithfully instead of collapsing to DECODE_ERROR.
            Some(Err((code, msg))) => Err(ThumbnailError::new(code, msg)),
            None => Err(ThumbnailError::new(
                "IO_ERROR",
                "Thumbnail task produced no result",
            )),
        };
    }

    // Acquire concurrency semaphore.
    let permit: OwnedSemaphorePermit = Arc::clone(&state.semaphore)
        .acquire_owned()
        .await
        .map_err(|_| ThumbnailError::new("IO_ERROR", "Semaphore closed"))?;

    // Generate thumbnail on a blocking thread.
    let cache_dir_clone = cache_dir.clone();
    let cache_file_clone = cache_file.clone();
    let cache_key_clone = cache_key.clone();
    let path_clone = path.clone();

    let result: Result<PathBuf, GenError> = tauri::async_runtime::spawn_blocking(move || {
        // permit is dropped when this closure ends, releasing the semaphore slot.
        let _permit: OwnedSemaphorePermit = permit;
        generate_thumbnail(&path_clone, &cache_dir_clone, &cache_file_clone, size)
    })
    .await
    .map_err(|e| GenError::IoFailed(format!("Thumbnail task panicked: {e}")))
    .and_then(|r| r);

    // Notify waiters with the semantically correct error code (M2).
    {
        let mut map = state.in_flight.lock().unwrap();
        if let Some(tx) = map.remove(&cache_key_clone) {
            let notify_value: InFlightResult = match &result {
                Ok(p) => Some(Ok(p.clone())),
                Err(GenError::ImageTooLarge) => Some(Err((
                    "IMAGE_TOO_LARGE",
                    format!("Image side exceeds {MAX_SIDE_PIXELS} pixels; thumbnail skipped"),
                ))),
                Err(GenError::DecodeFailed(msg)) => Some(Err(("DECODE_ERROR", msg.clone()))),
                Err(GenError::IoFailed(msg)) => Some(Err(("IO_ERROR", msg.clone()))),
            };
            let _ = tx.send(notify_value);
        }
    }

    match result {
        Ok(out_path) => {
            ensure_cache_scope(&app, &cache_dir)?;
            if fs::metadata(&out_path)
                .map(|metadata| state.record_disk_cache_write(metadata.len()))
                .unwrap_or(false)
            {
                let settings_path = app
                    .path()
                    .app_config_dir()
                    .ok()
                    .map(|directory| directory.join("settings.json"));
                spawn_disk_cache_sweep(cache_dir, settings_path);
            }
            Ok(out_path.to_string_lossy().into_owned())
        }
        Err(gen_err) => Err(gen_err.into_thumbnail_error()),
    }
}

/// Clear thumbnail disk cache; returns freed bytes.
#[tauri::command]
pub async fn clear_thumbnail_cache(app: AppHandle) -> Result<u64, ThumbnailError> {
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| {
            ThumbnailError::new("IO_ERROR", format!("Failed to get cache directory: {e}"))
        })?
        .join("thumbnails");

    if !cache_dir.exists() {
        return Ok(0);
    }

    let freed = tauri::async_runtime::spawn_blocking(move || {
        let mut total: u64 = 0;
        let entries =
            fs::read_dir(&cache_dir).map_err(|e| format!("Failed to read cache directory: {e}"))?;
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
    .map_err(|e| ThumbnailError::new("IO_ERROR", format!("Cache clear task panicked: {e}")))?
    .map_err(|e| ThumbnailError::new("IO_ERROR", e))?;

    Ok(freed)
}

// ──────────────────────────────────────────────────────────────────────────────
// Internal helpers (pub for test modules)
// ──────────────────────────────────────────────────────────────────────────────

/// Compute a stable cache key (first 16 bytes of SHA-256, hex-encoded = 32 chars).
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
    // Take first 16 bytes (128 bits) → 32 hex chars.
    let bytes: &[u8] = &digest[..16];
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 按文件 mtime 从旧到新淘汰缩略图缓存；超过 90% 时清理到 70%。
pub(crate) fn enforce_disk_cache_limit(cache_dir: &Path, limit_bytes: u64) -> u64 {
    if limit_bytes == 0 {
        return 0;
    }

    let Ok(entries) = fs::read_dir(cache_dir) else {
        return 0;
    };
    let mut files = Vec::new();
    let mut total = 0u64;
    for entry in entries.filter_map(Result::ok) {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let size = metadata.len();
        total = total.saturating_add(size);
        files.push((
            entry.path(),
            size,
            metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
        ));
    }

    let high = limit_bytes.saturating_mul(90) / 100;
    if total <= high {
        return 0;
    }
    let low = limit_bytes.saturating_mul(70) / 100;
    files.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));

    let mut freed = 0u64;
    for (path, size, _) in files {
        if total <= low {
            break;
        }
        if fs::remove_file(path).is_ok() {
            total = total.saturating_sub(size);
            freed = freed.saturating_add(size);
        }
    }
    freed
}

/// 长会话后台检查：每次触发时重新读取磁盘缓存上限，以响应运行期间的设置变化。
fn spawn_disk_cache_sweep(cache_dir: PathBuf, settings_path: Option<PathBuf>) {
    tauri::async_runtime::spawn_blocking(move || {
        let settings = settings_path
            .as_deref()
            .and_then(|path| read_settings_file(path).ok())
            .unwrap_or_default();
        let limit_bytes = settings
            .cache
            .disk_cache_limit_mb
            .saturating_mul(1024 * 1024);
        enforce_disk_cache_limit(&cache_dir, limit_bytes)
    });
}

/// Generate a thumbnail and write it to disk; returns the cache file path.
pub fn generate_thumbnail(
    src_path: &str,
    cache_dir: &Path,
    cache_file: &Path,
    size: u32,
) -> Result<PathBuf, GenError> {
    // Ensure cache directory exists.
    fs::create_dir_all(cache_dir)
        .map_err(|e| GenError::IoFailed(format!("Failed to create cache directory: {e}")))?;

    let path = Path::new(src_path);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let system_decode_dir = cache_dir
        .parent()
        .map(|directory| directory.join("system-decode"));

    // 1. 仅读 header 取尺寸（系统格式 sips 元数据；普通格式 image-rs header）。
    let (w, h) = if extended_formats::is_system_decoded(path) {
        extended_formats::probe_system_image(path).map_err(|error| {
            if error.starts_with("IMAGE_TOO_LARGE:") {
                // 系统格式（TIFF/HEIC/RAW）超限暂仍跳过；常规大图为非系统格式，走下方 sips 降采样。
                GenError::ImageTooLarge
            } else {
                GenError::DecodeFailed(error)
            }
        })?
    } else {
        let reader = ImageReader::open(path)
            .map_err(|e| GenError::IoFailed(format!("Failed to open image file: {e}")))?
            .with_guessed_format()
            .map_err(|e| GenError::IoFailed(format!("Failed to guess image format: {e}")))?;
        // into_dimensions() 仅读 header，不整图解码。
        reader
            .into_dimensions()
            .map_err(|e| GenError::DecodeFailed(format!("Failed to read image dimensions: {e}")))?
    };

    // 2. 取缩略源：超限（单边 > MAX_SIDE_PIXELS 或像素 > MAX_INPROC_PIXELS）改用 sips
    //    子进程降采样（解码在 sips 进程内、内存隔离），否则本进程解码。
    let oversized =
        w > MAX_SIDE_PIXELS || h > MAX_SIDE_PIXELS || (w as u64 * h as u64) > MAX_INPROC_PIXELS;
    let img = if oversized {
        extended_formats::downscale_with_sips_in(path, size, system_decode_dir.as_deref())
            .map_err(GenError::DecodeFailed)?
    } else {
        // 系统/ColorSync 链路直接读路径（避免对 HEIC/RAW 再整文件读入内存）。
        let raw = if extended_formats::needs_colorsync_output(path) {
            Vec::new()
        } else {
            fs::read(path)
                .map_err(|e| GenError::IoFailed(format!("Failed to read image file: {e}")))?
        };
        decode_image_in(&raw, &ext, path, system_decode_dir.as_deref())
            .map_err(|e| GenError::DecodeFailed(format!("Failed to decode image: {e}")))?
    };

    // 3. Resize to fit within size×size.
    let thumb = img.thumbnail(size, size);

    // Encode as WebP (image 0.25 built-in support).
    let mut buf = Cursor::new(Vec::new());
    thumb
        .write_to(&mut buf, ImageFormat::WebP)
        .map_err(|e| GenError::IoFailed(format!("Failed to encode WebP: {e}")))?;

    // Atomic write (write to .tmp, then rename).
    let tmp = cache_file.with_extension("tmp");
    fs::write(&tmp, buf.into_inner())
        .map_err(|e| GenError::IoFailed(format!("Failed to write temp cache file: {e}")))?;
    // Minor 11: attempt to remove .tmp on rename failure to avoid leftover files.
    fs::rename(&tmp, cache_file).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        GenError::IoFailed(format!("Failed to replace cache file: {e}"))
    })?;

    Ok(cache_file.to_path_buf())
}

/// Decode image; for JPEG/WebP/PNG apply EXIF orientation when available.
pub fn decode_image(raw: &[u8], ext: &str, path: &Path) -> Result<DynamicImage, String> {
    decode_image_in(raw, ext, path, None)
}

fn decode_image_in(
    raw: &[u8],
    ext: &str,
    path: &Path,
    system_decode_dir: Option<&Path>,
) -> Result<DynamicImage, String> {
    if extended_formats::needs_colorsync_output(path) {
        return extended_formats::decode_system_image_in(path, system_decode_dir);
    }
    // GIF: take only the first frame (image crate default).
    if ext == "gif" {
        let img = image::load_from_memory_with_format(raw, ImageFormat::Gif)
            .map_err(|e| format!("Failed to decode GIF: {e}"))?;
        return Ok(img);
    }

    let reader = ImageReader::new(Cursor::new(raw))
        .with_guessed_format()
        .map_err(|e| format!("Failed to guess image format ({}): {e}", path.display()))?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("Failed to create image decoder ({}): {e}", path.display()))?;
    let icc = decoder.icc_profile().unwrap_or(None);
    let mut img = DynamicImage::from_decoder(decoder)
        .map_err(|e| format!("Failed to decode image ({}): {e}", path.display()))?;
    if let Some(icc) = icc {
        img = color::dynamic_image_to_srgb(img, &icc);
    }

    // Minor 3: apply EXIF orientation for any container kamadak-exif supports
    // (JPEG, WebP, PNG with Exif chunk, TIFF, etc.); silently ignored when absent.
    let oriented = apply_exif_orientation(img, raw);
    Ok(oriented)
}

/// Read EXIF Orientation and rotate/flip the image accordingly (EXIF orientation 1-8).
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
        _ => img, // 1 or unknown: no rotation
    }
}

/// Read EXIF Orientation value (1-8) from raw image bytes.
/// Works for any container supported by kamadak-exif (JPEG, WebP, PNG Exif chunk, TIFF…).
/// Returns None when no EXIF data is found; caller should treat as orientation 1.
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

/// Grant the cache directory access in the asset protocol scope (idempotent).
fn ensure_cache_scope(app: &AppHandle, cache_dir: &Path) -> Result<(), ThumbnailError> {
    app.asset_protocol_scope()
        .allow_directory(cache_dir, false)
        .map_err(|e| {
            ThumbnailError::new(
                "IO_ERROR",
                format!("Failed to authorize cache directory: {e}"),
            )
        })
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::{set_file_mtime, FileTime};
    use image::GenericImageView;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Helper: create a minimal valid JPEG in memory using the image crate.
    fn make_jpeg_bytes(width: u32, height: u32) -> Vec<u8> {
        use image::{ImageBuffer, ImageFormat, Rgb};
        let img: ImageBuffer<Rgb<u8>, _> =
            ImageBuffer::from_fn(width, height, |x, _| Rgb([x as u8, 50u8, 100u8]));
        let dyn_img = image::DynamicImage::ImageRgb8(img);
        let mut buf = std::io::Cursor::new(Vec::new());
        dyn_img
            .write_to(&mut buf, ImageFormat::Jpeg)
            .expect("JPEG encoding should succeed");
        buf.into_inner()
    }

    fn write_cache_file(cache_dir: &Path, name: &str, size: usize, mtime_seconds: i64) {
        let path = cache_dir.join(name);
        fs::write(&path, vec![0u8; size]).unwrap();
        set_file_mtime(path, FileTime::from_unix_time(mtime_seconds, 0)).unwrap();
    }

    #[test]
    fn test_enforce_disk_cache_limit_deletes_oldest_to_low_watermark() {
        let cache_dir = tempfile::tempdir().unwrap();
        write_cache_file(cache_dir.path(), "oldest.webp", 30, 1);
        write_cache_file(cache_dir.path(), "older.webp", 30, 2);
        write_cache_file(cache_dir.path(), "newer.webp", 30, 3);
        write_cache_file(cache_dir.path(), "newest.webp", 30, 4);

        let freed = enforce_disk_cache_limit(cache_dir.path(), 100);

        assert_eq!(freed, 60);
        assert!(!cache_dir.path().join("oldest.webp").exists());
        assert!(!cache_dir.path().join("older.webp").exists());
        assert!(cache_dir.path().join("newer.webp").exists());
        assert!(cache_dir.path().join("newest.webp").exists());
    }

    #[test]
    fn test_enforce_disk_cache_limit_does_nothing_below_high_watermark() {
        let cache_dir = tempfile::tempdir().unwrap();
        write_cache_file(cache_dir.path(), "old.webp", 40, 1);
        write_cache_file(cache_dir.path(), "new.webp", 50, 2);

        assert_eq!(enforce_disk_cache_limit(cache_dir.path(), 100), 0);
        assert!(cache_dir.path().join("old.webp").exists());
        assert!(cache_dir.path().join("new.webp").exists());
    }

    #[test]
    fn test_enforce_disk_cache_limit_zero_is_unlimited() {
        let cache_dir = tempfile::tempdir().unwrap();
        write_cache_file(cache_dir.path(), "cached.webp", 100, 1);

        assert_eq!(enforce_disk_cache_limit(cache_dir.path(), 0), 0);
        assert!(cache_dir.path().join("cached.webp").exists());
    }

    #[test]
    fn test_thumbnail_state_triggers_sweep_after_write_threshold() {
        let state = ThumbnailState::new(1);
        assert!(!state.record_disk_cache_write(DISK_CACHE_SWEEP_WRITE_THRESHOLD_BYTES - 1));
        assert!(state.record_disk_cache_write(1));
        assert!(!state.record_disk_cache_write(1));
    }

    #[test]
    fn test_compute_cache_key_deterministic() {
        let path = Path::new("/tmp/test.jpg");
        let k1 = compute_cache_key(path, 12345, 9999, 96);
        let k2 = compute_cache_key(path, 12345, 9999, 96);
        assert_eq!(k1, k2, "Cache key must be deterministic");
        assert_eq!(k1.len(), 32, "Cache key must be 32 hex chars");
    }

    #[test]
    fn test_compute_cache_key_differs_on_size() {
        let path = Path::new("/tmp/test.jpg");
        let k96 = compute_cache_key(path, 100, 0, 96);
        let k160 = compute_cache_key(path, 100, 0, 160);
        assert_ne!(
            k96, k160,
            "Different sizes must produce different cache keys"
        );
    }

    #[test]
    fn test_exif_orientation_no_exif_returns_none() {
        // PNG bytes with no EXIF — should return None gracefully.
        let png_bytes = include_bytes!("../tests/fixtures/1x1.png");
        let result = read_exif_orientation(png_bytes);
        // No EXIF → None (treated as orientation 1 by caller).
        assert!(result.is_none() || result == Some(1));
    }

    #[test]
    fn test_apply_exif_orientation_identity() {
        // Orientation 1 (or no EXIF) → image unchanged.
        let jpeg = make_jpeg_bytes(4, 4);
        let img = image::load_from_memory(&jpeg).expect("Failed to load test JPEG");
        let (w, h) = img.dimensions();
        let oriented = apply_exif_orientation(img, &jpeg);
        assert_eq!(
            oriented.dimensions(),
            (w, h),
            "Orientation 1 must not change dimensions"
        );
    }

    #[test]
    fn test_jpeg_decode_uses_memory_not_sips_path() {
        let jpeg = make_jpeg_bytes(8, 6);
        let decoded = decode_image(&jpeg, "jpg", Path::new("/nonexistent/plain.jpg"))
            .expect("JPEG 应直接从内存由 image-rs 解码");
        assert_eq!(decoded.dimensions(), (8, 6));
    }

    #[test]
    fn test_generate_thumbnail_image_too_large() {
        // Create a real tiny JPEG and verify successful thumbnail generation.
        let cache_dir = tempfile::tempdir().expect("tempdir");
        let cache_file = cache_dir.path().join("out.webp");

        // Write a real tiny JPEG to a temp file.
        let jpeg = make_jpeg_bytes(8, 8);
        let mut tmp = NamedTempFile::with_suffix(".jpg").expect("NamedTempFile");
        tmp.write_all(&jpeg).expect("write JPEG");

        let result = generate_thumbnail(
            tmp.path().to_str().unwrap(),
            cache_dir.path(),
            &cache_file,
            96,
        );
        assert!(
            result.is_ok(),
            "Tiny JPEG should generate thumbnail successfully: {result:?}"
        );
    }

    #[test]
    fn test_generate_thumbnail_returns_image_too_large_error() {
        // Verify that GenError::ImageTooLarge maps to code IMAGE_TOO_LARGE.
        let err = GenError::ImageTooLarge.into_thumbnail_error();
        assert_eq!(err.code, "IMAGE_TOO_LARGE");
    }

    #[test]
    fn test_generate_thumbnail_returns_decode_error() {
        // Verify that GenError::DecodeFailed maps to code DECODE_ERROR.
        let err = GenError::DecodeFailed("bad data".into()).into_thumbnail_error();
        assert_eq!(err.code, "DECODE_ERROR");
    }

    #[test]
    fn test_generate_thumbnail_returns_io_error() {
        // Verify that GenError::IoFailed maps to code IO_ERROR.
        let err = GenError::IoFailed("disk full".into()).into_thumbnail_error();
        assert_eq!(err.code, "IO_ERROR");
    }

    #[test]
    fn test_generate_thumbnail_bad_file_returns_io_error() {
        let cache_dir = tempfile::tempdir().expect("tempdir");
        let cache_file = cache_dir.path().join("out.webp");
        let result = generate_thumbnail(
            "/nonexistent/path/image.jpg",
            cache_dir.path(),
            &cache_file,
            96,
        );
        assert!(result.is_err());
        match result.unwrap_err() {
            GenError::IoFailed(_) => {}
            other => panic!("Expected IoFailed, got {other:?}"),
        }
    }

    #[test]
    fn test_generate_thumbnail_corrupted_bytes_returns_decode_error() {
        let cache_dir = tempfile::tempdir().expect("tempdir");
        let cache_file = cache_dir.path().join("out.webp");

        // Write garbage bytes as a .jpg file.
        let mut tmp = NamedTempFile::with_suffix(".jpg").expect("NamedTempFile");
        tmp.write_all(b"not a real jpeg at all garbage bytes")
            .expect("write");

        let result = generate_thumbnail(
            tmp.path().to_str().unwrap(),
            cache_dir.path(),
            &cache_file,
            96,
        );
        assert!(result.is_err());
        // Could be DecodeFailed (dimension read fails on bad data).
        match result.unwrap_err() {
            GenError::DecodeFailed(_) | GenError::IoFailed(_) => {}
            GenError::ImageTooLarge => panic!("Should not be ImageTooLarge for garbage bytes"),
        }
    }

    #[test]
    #[ignore]
    fn benchmark_jpeg_decode() {
        let jpeg = make_jpeg_bytes(4000, 3000);
        let start = std::time::Instant::now();
        let decoded = decode_image(&jpeg, "jpg", Path::new("benchmark.jpg")).unwrap();
        println!(
            "JPEG image-rs decode {}×{}: {}ms",
            decoded.width(),
            decoded.height(),
            start.elapsed().as_millis()
        );
    }
}
