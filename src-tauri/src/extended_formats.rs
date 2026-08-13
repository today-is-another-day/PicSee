use crate::system_decode_backend as backend;
use image::DynamicImage;
use lru::LruCache;
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
    time::{Duration, Instant, UNIX_EPOCH},
};
use tauri::{AppHandle, Manager};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(1);
static SYSTEM_DECODE_CACHE: OnceLock<Mutex<LruCache<CacheKey, CachedDecode>>> = OnceLock::new();
static SYSTEM_PREFETCH_SEMAPHORE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
const DECODE_TIMEOUT: Duration = Duration::from_secs(30);
const SYSTEM_DECODE_CACHE_CAPACITY: usize = 6;
pub const SYSTEM_MAX_SIDE_PIXELS: u32 = 12_000;
pub const SYSTEM_MAX_DECODE_BYTES: u64 = 512 * 1024 * 1024;
/// 超过安全上限的系统格式（常见于大幅面扫描 TIFF）降采样到的最长边。
/// 11000² × 4B ≈ 484MB，落在 [`SYSTEM_MAX_DECODE_BYTES`] 之内。
pub const SYSTEM_OVERSIZED_DECODE_MAX_SIDE: u32 = 11_000;

type CacheKey = (PathBuf, i64);

#[derive(Clone)]
struct CachedDecode {
    /// 源图真实尺寸（可能大于缓存 PNG 的尺寸：超限时缓存的是降采样图）。
    source_width: u32,
    source_height: u32,
    png_path: PathBuf,
}

pub const TIFF_EXTENSIONS: [&str; 2] = ["tif", "tiff"];
pub const SYSTEM_EXTENSIONS: [&str; 12] = [
    "heic", "heif", "avif", "jxl", "dng", "cr2", "cr3", "nef", "arw", "raf", "orf", "rw2",
];
pub const RAW_EXTENSIONS: [&str; 8] = ["dng", "cr2", "cr3", "nef", "arw", "raf", "orf", "rw2"];

pub fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

pub fn is_tiff(path: &Path) -> bool {
    TIFF_EXTENSIONS.contains(&extension(path).as_str())
}

/// JPEG XL：ImageIO（macOS 14+）可解码，但 WebKit 对 JXL 的支持在各版本间反复，
/// 因此不走 `<img>` 直显，统一交给系统解码 + 大图管线，避免"先失败再回退"的闪烁。
pub fn is_jxl(path: &Path) -> bool {
    extension(path) == "jxl"
}

/// 已知 HEIF/HEIC/AVIF 的 ISO-BMFF 品牌（major + compatible brands）。
const HEIF_BRANDS: [&[u8; 4]; 12] = [
    b"heic", b"heix", b"heim", b"heis", b"hevc", b"hevx", b"hevm", b"hevs", b"mif1", b"msf1",
    b"avif", b"avis",
];

/// 通过文件头嗅探 ISO-BMFF（HEIF/HEIC/AVIF）容器，以处理扩展名与内容不符的情况
/// （例如 iPhone 导出的 `.png` 实为 HEIF，需走系统解码而非 image-rs）。
pub fn is_heif_content(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut header = [0u8; 32];
    let Ok(read) = file.read(&mut header) else {
        return false;
    };
    // ISO-BMFF：bytes[4..8] == "ftyp"，major brand 在 [8..12]，compatible brands 从 16 起每 4 字节。
    if read < 12 || &header[4..8] != b"ftyp" {
        return false;
    }
    if HEIF_BRANDS
        .iter()
        .any(|brand| brand.as_slice() == &header[8..12])
    {
        return true;
    }
    let mut offset = 16;
    while offset + 4 <= read {
        if HEIF_BRANDS
            .iter()
            .any(|brand| brand.as_slice() == &header[offset..offset + 4])
        {
            return true;
        }
        offset += 4;
    }
    false
}

pub fn is_system_decoded(path: &Path) -> bool {
    is_tiff(path) || SYSTEM_EXTENSIONS.contains(&extension(path).as_str()) || is_heif_content(path)
}

pub fn is_raw(path: &Path) -> bool {
    RAW_EXTENSIONS.contains(&extension(path).as_str())
}

/// 只有 WebView 无法直接显示的系统格式才走 ColorSync 子进程。
pub fn needs_colorsync_output(path: &Path) -> bool {
    is_system_decoded(path)
}

/// 当前解码后端不支持该格式时返回明确错误（不启动子进程）。
///
/// macOS（sips/ImageIO）覆盖全部系统格式，永远放行；
/// 非 macOS（libvips）暂不支持相机 RAW，此处提前优雅报错，避免子进程产生晦涩失败。
#[cfg(target_os = "macos")]
fn ensure_backend_supports(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn ensure_backend_supports(path: &Path) -> Result<(), String> {
    if is_raw(path) {
        return Err(format!(
            "RAW_UNSUPPORTED: 当前平台暂不支持相机 RAW 格式（.{}），请转换为 JPEG/PNG/TIFF/HEIC 后打开",
            extension(path)
        ));
    }
    Ok(())
}

/// 使用 macOS ImageIO/ColorSync 解码为 PNG，再交给 image-rs 消费。
///
/// 源图超出安全上限（[`SYSTEM_MAX_SIDE_PIXELS`] / [`SYSTEM_MAX_DECODE_BYTES`]，
/// 常见于大幅面扫描 TIFF）时不再拒绝打开，而是在子进程内降采样到
/// [`SYSTEM_OVERSIZED_DECODE_MAX_SIDE`]（解码内存隔离在子进程），返回降采样图；
/// 源图真实尺寸仍可通过 [`probe_system_dimensions`] 获取。
///
/// `preferred_directory` 应传入 Tauri app cache 目录；纯函数/测试调用回退系统临时目录。
pub fn decode_system_image_in(
    path: &Path,
    preferred_directory: Option<&Path>,
) -> Result<DynamicImage, String> {
    let cache_key = system_decode_cache_key(path);
    if let Some(cached) = cache_key.as_ref().and_then(get_cached_decode) {
        if let Ok(decoded) = image::open(&cached.png_path) {
            return Ok(decoded);
        }
        remove_cached_decode(cache_key.as_ref().unwrap());
    }

    // 未命中缓存的入口先读 header 尺寸，决定走全量解码还是子进程降采样。
    let (source_width, source_height) = probe_system_source_dimensions(path)?;
    let oversized = validate_system_dimensions(source_width, source_height).is_err();
    let directory = runtime_decode_directory(preferred_directory)?;
    let temporary_output = temporary_png_path(&directory);
    let cached_output = cache_key
        .as_ref()
        .map(|key| cached_png_path(&directory, key));
    let mut command = if oversized {
        backend::downscale_command(path, SYSTEM_OVERSIZED_DECODE_MAX_SIDE, &temporary_output)?
    } else {
        backend::decode_command(path, &temporary_output)?
    };

    record_system_decode(path);
    let result = run_command_with_timeout(&mut command, DECODE_TIMEOUT);
    let decoded = match result {
        Ok(result) if result.status.success() => match image::open(&temporary_output) {
            Ok(decoded) => {
                if let (Some(key), Some(png_path)) = (cache_key, cached_output) {
                    if let Err(error) = std::fs::rename(&temporary_output, &png_path) {
                        let _ = std::fs::remove_file(&temporary_output);
                        return Err(format!("保存系统解码缓存 PNG 失败: {error}"));
                    }
                    insert_global_cached_decode(
                        key,
                        CachedDecode {
                            source_width,
                            source_height,
                            png_path,
                        },
                    );
                } else {
                    let _ = std::fs::remove_file(&temporary_output);
                }
                Ok(decoded)
            }
            Err(error) => {
                let _ = std::fs::remove_file(&temporary_output);
                Err(format!("读取系统解码 PNG 失败: {error}"))
            }
        },
        Ok(result) => {
            let _ = std::fs::remove_file(&temporary_output);
            Err(format!(
                "{} 无法解码此格式: {}",
                backend::NAME,
                String::from_utf8_lossy(&result.stderr)
            ))
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temporary_output);
            Err(error)
        }
    };
    decoded
}

pub fn decode_system_image(path: &Path) -> Result<DynamicImage, String> {
    decode_system_image_in(path, None)
}

/// 用子进程把任意大图降采样为最长边 ≤ `max_side` 的临时 PNG，再交给 image-rs。
///
/// 关键价值：解码在外部解码器进程内完成（内存隔离），本进程只读取降采样后的小图，
/// 避免对超大图（> 1 亿像素 / 超长边）在本进程整图解码导致 OOM。
pub fn downscale_in(
    path: &Path,
    max_side: u32,
    preferred_directory: Option<&Path>,
) -> Result<DynamicImage, String> {
    ensure_backend_supports(path)?;
    let directory = runtime_decode_directory(preferred_directory)?;
    let output = temporary_png_path(&directory);
    let mut command = backend::downscale_command(path, max_side, &output)?;

    let result = run_command_with_timeout(&mut command, DECODE_TIMEOUT);
    let decoded = match result {
        Ok(result) if result.status.success() => {
            image::open(&output).map_err(|error| format!("读取降采样 PNG 失败: {error}"))
        }
        Ok(result) => Err(format!(
            "{} 降采样失败: {}",
            backend::NAME,
            String::from_utf8_lossy(&result.stderr)
        )),
        Err(error) => Err(error),
    };
    let _ = std::fs::remove_file(&output);
    decoded
}

/// 把系统格式直接解码为未压缩 BMP 栅格落盘，供大图引擎按需分块读取。
///
/// 与 [`decode_system_image_in`] 的区别：整图像素从不进入本进程内存——
/// 解码在外部解码器进程内完成并直接写成 BMP，随后由 `BmpReader` 随机读取瓦片。
/// `max_side` 为 `Some(n)` 时同时降采样到最长边 ≤ n。
///
/// 当前解码后端不支持写 BMP（如 Linux libvips）时返回 `Ok(false)`，调用方应回退到预览路径。
pub fn decode_system_image_to_bmp(
    path: &Path,
    max_side: Option<u32>,
    output: &Path,
) -> Result<bool, String> {
    ensure_backend_supports(path)?;
    let Some(mut command) = backend::raster_bmp_command(path, max_side, output) else {
        return Ok(false);
    };
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建 BMP 栅格目录失败: {error}"))?;
    }

    record_system_decode(path);
    match run_command_with_timeout(&mut command, DECODE_TIMEOUT) {
        Ok(result) if result.status.success() => Ok(true),
        Ok(result) => {
            let _ = std::fs::remove_file(output);
            Err(format!(
                "{} 无法解码为 BMP 栅格: {}",
                backend::NAME,
                String::from_utf8_lossy(&result.stderr)
            ))
        }
        Err(error) => {
            let _ = std::fs::remove_file(output);
            Err(error)
        }
    }
}

/// 仅通过后端元数据读取尺寸，不生成临时 PNG、不全量解码；超限直接报 `IMAGE_TOO_LARGE`。
pub fn probe_system_image(path: &Path) -> Result<(u32, u32), String> {
    let (width, height) = probe_system_source_dimensions(path)?;
    validate_system_dimensions(width, height)?;
    Ok((width, height))
}

/// 读取源图真实尺寸，不施加解码安全上限。
///
/// 供“需要知道尺寸但不会在本进程整图解码”的调用方使用
/// （超限时由 [`decode_system_image_in`] / [`downscale_in`] 在子进程内降采样）。
pub fn probe_system_source_dimensions(path: &Path) -> Result<(u32, u32), String> {
    ensure_backend_supports(path)?;
    let mut command = backend::probe_command(path)?;
    record_system_probe(path);
    let output = run_command_with_timeout(&mut command, DECODE_TIMEOUT)?;
    if !output.status.success() {
        return Err(format!(
            "{} 无法读取图像尺寸: {}",
            backend::NAME,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    backend::parse_probe_output(&stdout)
}

/// 优先复用系统解码缓存中的源图尺寸；未命中时才调用后端元数据探测。
///
/// 返回源图真实尺寸，不因超出解码安全上限而失败——超大图仍可按降采样预览打开。
pub fn probe_system_dimensions(path: &Path) -> Result<(u32, u32), String> {
    if let Some(cached) = system_decode_cache_key(path)
        .as_ref()
        .and_then(get_cached_decode)
    {
        return Ok((cached.source_width, cached.source_height));
    }
    probe_system_source_dimensions(path)
}

/// 判断当前文件版本是否已有可读的系统解码缓存。
pub fn is_system_decode_cached(path: &Path) -> bool {
    system_decode_cache_key(path)
        .as_ref()
        .and_then(get_cached_decode)
        .is_some()
}

/// 顺序预解码邻图；全局单通道避免快速切图时同时启动多个 sips 子进程。
///
/// 只预热"会用到整图系统解码"的中小图（`<img>` 直显失败后的回退、缩略图）。
/// 大图走大图引擎的 BMP 栅格 + 分块管线，整图 PNG 缓存对它毫无用处，
/// 预热反而会抢占 CPU 并在缓存里堆积上百 MB 的临时 PNG。
#[tauri::command]
pub async fn prefetch_system_decode(app: AppHandle, paths: Vec<String>) -> Result<(), String> {
    let _permit = SYSTEM_PREFETCH_SEMAPHORE
        .acquire()
        .await
        .map_err(|error| format!("系统解码预取信号量已关闭: {error}"))?;
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|error| format!("无法获取应用缓存目录: {error}"))?
        .join("system-decode");
    let settings = app
        .path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("settings.json"))
        .as_deref()
        .and_then(|path| crate::settings::read_settings_file(path).ok())
        .unwrap_or_default()
        .large_image;
    let file_size_threshold = settings
        .file_size_threshold_mb
        .saturating_mul(1024 * 1024);

    for path in paths {
        let path = PathBuf::from(path);
        if !is_system_decoded(&path) || is_system_decode_cached(&path) {
            continue;
        }
        let file_size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        if file_size >= file_size_threshold {
            continue;
        }
        // 高压缩比格式（AVIF/HEIC）文件小也可能是上亿像素，同样交给大图引擎。
        if let Ok((width, height)) = probe_system_source_dimensions(&path) {
            if width as u64 * height as u64 >= settings.pixel_threshold {
                continue;
            }
        }
        let cache_dir = cache_dir.clone();
        let _ = tauri::async_runtime::spawn_blocking(move || {
            let _ = decode_system_image_in(&path, Some(&cache_dir));
        })
        .await;
    }
    Ok(())
}

pub fn validate_system_dimensions(width: u32, height: u32) -> Result<(), String> {
    let decoded_bytes = width as u64 * height as u64 * 4;
    if width > SYSTEM_MAX_SIDE_PIXELS
        || height > SYSTEM_MAX_SIDE_PIXELS
        || decoded_bytes > SYSTEM_MAX_DECODE_BYTES
    {
        return Err(format!(
            "IMAGE_TOO_LARGE: {width}x{height} exceeds the system decode safety limit"
        ));
    }
    Ok(())
}

/// 返回运行期可写目录；绝不依赖构建机源码路径。
pub fn runtime_decode_directory(preferred_directory: Option<&Path>) -> Result<PathBuf, String> {
    let directory = preferred_directory
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::temp_dir().join("picsee-system-decode"));
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("创建系统解码临时目录失败: {error}"))?;
    Ok(directory)
}

fn temporary_png_path(directory: &Path) -> PathBuf {
    let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    directory.join(format!(
        "picsee-system-decode-{}-{id}.png",
        std::process::id()
    ))
}

fn cached_png_path(directory: &Path, key: &CacheKey) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    directory.join(format!("picsee-system-decode-{:016x}.png", hasher.finish()))
}

fn system_decode_cache_key(path: &Path) -> Option<CacheKey> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let nanos = modified.duration_since(UNIX_EPOCH).ok()?.as_nanos();
    let nanos = i64::try_from(nanos).ok()?;
    Some((path.to_path_buf(), nanos))
}

fn new_system_decode_cache(capacity: usize) -> LruCache<CacheKey, CachedDecode> {
    LruCache::new(NonZeroUsize::new(capacity).expect("系统解码缓存容量必须大于 0"))
}

fn system_decode_cache() -> &'static Mutex<LruCache<CacheKey, CachedDecode>> {
    SYSTEM_DECODE_CACHE
        .get_or_init(|| Mutex::new(new_system_decode_cache(SYSTEM_DECODE_CACHE_CAPACITY)))
}

fn get_cached_decode(key: &CacheKey) -> Option<CachedDecode> {
    let mut cache = system_decode_cache().lock().unwrap();
    let cached = cache.get(key)?.clone();
    if cached.png_path.exists() {
        Some(cached)
    } else {
        cache.pop(key);
        None
    }
}

fn remove_cached_decode(key: &CacheKey) {
    if let Some(cached) = system_decode_cache().lock().unwrap().pop(key) {
        let _ = std::fs::remove_file(cached.png_path);
    }
}

fn insert_global_cached_decode(key: CacheKey, cached: CachedDecode) {
    let mut cache = system_decode_cache().lock().unwrap();
    insert_cached_decode(&mut cache, key, cached);
}

fn insert_cached_decode(
    cache: &mut LruCache<CacheKey, CachedDecode>,
    key: CacheKey,
    cached: CachedDecode,
) {
    let new_png_path = cached.png_path.clone();
    if let Some((_, evicted)) = cache.push(key, cached) {
        if evicted.png_path != new_png_path {
            let _ = std::fs::remove_file(evicted.png_path);
        }
    }
}

#[cfg(test)]
fn test_command_counts() -> &'static Mutex<std::collections::HashMap<(PathBuf, &'static str), u64>>
{
    static COUNTS: OnceLock<Mutex<std::collections::HashMap<(PathBuf, &'static str), u64>>> =
        OnceLock::new();
    COUNTS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

#[cfg(test)]
fn record_test_command(path: &Path, command: &'static str) {
    *test_command_counts()
        .lock()
        .unwrap()
        .entry((path.to_path_buf(), command))
        .or_default() += 1;
}

#[cfg(all(test, target_os = "macos"))]
fn system_decode_count(path: &Path) -> u64 {
    test_command_counts()
        .lock()
        .unwrap()
        .get(&(path.to_path_buf(), "decode"))
        .copied()
        .unwrap_or_default()
}

#[cfg(all(test, target_os = "macos"))]
fn system_probe_count(path: &Path) -> u64 {
    test_command_counts()
        .lock()
        .unwrap()
        .get(&(path.to_path_buf(), "probe"))
        .copied()
        .unwrap_or_default()
}

fn record_system_decode(_path: &Path) {
    #[cfg(test)]
    record_test_command(_path, "decode");
}

fn record_system_probe(_path: &Path) {
    #[cfg(test)]
    record_test_command(_path, "probe");
}

fn run_command_with_timeout(command: &mut Command, timeout: Duration) -> Result<Output, String> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动{}解码子进程: {error}", backend::NAME))?;
    let started = Instant::now();
    loop {
        match child
            .try_wait()
            .map_err(|error| format!("等待{}解码失败: {error}", backend::NAME))?
        {
            Some(_) => {
                return child
                    .wait_with_output()
                    .map_err(|error| format!("读取{}输出失败: {error}", backend::NAME));
            }
            None if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{}解码超时（{} 秒）",
                    backend::NAME,
                    timeout.as_secs_f32()
                ));
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    use image::{GenericImageView, ImageBuffer, Rgb};
    use std::io::Write;

    #[cfg(target_os = "macos")]
    fn write_compressed_tiff(path: &Path, compression: &str) {
        let script = r#"
from PIL import Image
import sys
Image.new("RGB", (8, 6), (120, 40, 200)).save(sys.argv[1], format="TIFF", compression=sys.argv[2])
"#;
        let output = Command::new("python3")
            .args(["-c", script])
            .arg(path)
            .arg(compression)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn raw_and_tiff_extension_classification() {
        assert!(is_raw(Path::new("sample.cr3")));
        assert!(is_raw(Path::new("sample.NEF")));
        assert!(!is_raw(Path::new("sample.heic")));
        assert!(is_tiff(Path::new("sample.tiff")));
        assert!(is_tiff(Path::new("sample.TIF")));
    }

    #[test]
    fn only_system_formats_need_colorsync_subprocess() {
        assert!(!needs_colorsync_output(Path::new("sample.jpg")));
        assert!(!needs_colorsync_output(Path::new("sample.png")));
        assert!(needs_colorsync_output(Path::new("sample.tiff")));
        assert!(needs_colorsync_output(Path::new("sample.heic")));
    }

    #[test]
    fn avif_is_system_decoded_but_not_raw() {
        assert!(is_system_decoded(Path::new("sample.avif")));
        assert!(is_system_decoded(Path::new("sample.AVIF")));
        assert!(!is_raw(Path::new("sample.avif")));
        assert!(needs_colorsync_output(Path::new("sample.avif")));
    }

    #[test]
    fn jxl_is_system_decoded_and_never_webview_rendered() {
        assert!(is_system_decoded(Path::new("sample.jxl")));
        assert!(is_system_decoded(Path::new("sample.JXL")));
        assert!(is_jxl(Path::new("sample.JXL")));
        assert!(!is_raw(Path::new("sample.jxl")));
        assert!(needs_colorsync_output(Path::new("sample.jxl")));
        // 其他系统格式不应被误判为 JXL（否则会白白丢失 <img> 直显快路径）。
        assert!(!is_jxl(Path::new("sample.avif")));
        assert!(!is_jxl(Path::new("sample.tiff")));
    }

    #[test]
    fn heif_content_is_detected_regardless_of_extension() {
        let dir = tempfile::tempdir().unwrap();

        // 扩展名是 .png，内容却是 HEIF（iPhone 导出常见）：ftyp + heic 品牌。
        let mislabeled = dir.path().join("IMG.png");
        let mut header = vec![0x00, 0x00, 0x00, 0x18];
        header.extend_from_slice(b"ftypheic");
        header.extend_from_slice(b"\0\0\0\0mif1heic");
        std::fs::write(&mislabeled, &header).unwrap();
        assert!(is_heif_content(&mislabeled));
        assert!(is_system_decoded(&mislabeled));
        assert!(needs_colorsync_output(&mislabeled));

        // 真正的 PNG 内容不应被误判。
        let real_png = dir.path().join("real.png");
        std::fs::write(&real_png, b"\x89PNG\r\n\x1a\n........").unwrap();
        assert!(!is_heif_content(&real_png));
        assert!(!is_system_decoded(&real_png));
    }

    #[test]
    fn runtime_directory_exists_and_is_writable() {
        let directory = runtime_decode_directory(None).unwrap();
        assert!(directory.starts_with(std::env::temp_dir()));
        let probe = directory.join(format!("picsee-write-probe-{}", std::process::id()));
        std::fs::File::create(&probe)
            .unwrap()
            .write_all(b"ok")
            .unwrap();
        std::fs::remove_file(probe).unwrap();

        let preferred_root = tempfile::tempdir().unwrap();
        let preferred = preferred_root.path().join("system-decode");
        assert_eq!(
            runtime_decode_directory(Some(&preferred)).unwrap(),
            preferred
        );
        assert!(preferred.is_dir());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn system_decode_cache_hits_without_decoding_twice() {
        let directory = tempfile::tempdir().unwrap();
        let tiff = directory.path().join("cached.tiff");
        let cache_dir = directory.path().join("system-decode");
        write_compressed_tiff(&tiff, "tiff_lzw");

        let first = decode_system_image_in(&tiff, Some(&cache_dir)).unwrap();
        let second = decode_system_image_in(&tiff, Some(&cache_dir)).unwrap();

        assert_eq!(first.dimensions(), second.dimensions());
        assert_eq!(system_decode_count(&tiff), 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cached_system_dimensions_do_not_probe_twice() {
        let directory = tempfile::tempdir().unwrap();
        let tiff = directory.path().join("cached-probe.tiff");
        let cache_dir = directory.path().join("system-decode");
        write_compressed_tiff(&tiff, "tiff_lzw");

        let decoded = decode_system_image_in(&tiff, Some(&cache_dir)).unwrap();
        let probes_after_decode = system_probe_count(&tiff);

        assert_eq!(
            probe_system_dimensions(&tiff).unwrap(),
            decoded.dimensions()
        );
        assert_eq!(system_probe_count(&tiff), probes_after_decode);
    }

    #[test]
    fn cache_eviction_removes_png_file() {
        let directory = tempfile::tempdir().unwrap();
        let mut cache = new_system_decode_cache(1);
        let first_png = directory.path().join("first.png");
        let second_png = directory.path().join("second.png");
        std::fs::write(&first_png, b"first").unwrap();
        std::fs::write(&second_png, b"second").unwrap();

        insert_cached_decode(
            &mut cache,
            (directory.path().join("first.tiff"), 1),
            CachedDecode {
                source_width: 8,
                source_height: 6,
                png_path: first_png.clone(),
            },
        );
        insert_cached_decode(
            &mut cache,
            (directory.path().join("second.tiff"), 2),
            CachedDecode {
                source_width: 8,
                source_height: 6,
                png_path: second_png.clone(),
            },
        );

        assert!(!first_png.exists());
        assert!(second_png.exists());
    }

    #[test]
    fn command_timeout_kills_hung_process() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 2"]);
        let started = Instant::now();
        let error = run_command_with_timeout(&mut command, Duration::from_millis(50)).unwrap_err();
        assert!(error.contains("超时"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn system_dimension_limit_rejects_oversized_images() {
        assert!(validate_system_dimensions(12_001, 10).is_err());
        assert!(validate_system_dimensions(12_000, 12_000).is_err());
        assert!(validate_system_dimensions(8_000, 8_000).is_ok());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn oversized_tiff_probe_rejects_but_decode_downscales() {
        let directory = tempfile::tempdir().unwrap();
        let tiff = directory.path().join("oversized.tiff");
        let script = r#"
from PIL import Image
import sys
Image.new("RGB", (12001, 1), (1, 2, 3)).save(sys.argv[1], format="TIFF", compression="tiff_lzw")
"#;
        assert!(Command::new("python3")
            .args(["-c", script])
            .arg(&tiff)
            .output()
            .unwrap()
            .status
            .success());
        let error = probe_system_image(&tiff).unwrap_err();
        assert!(error.starts_with("IMAGE_TOO_LARGE:"));

        // 但源图尺寸探测与解码都不应因超限失败：解码走子进程降采样，仍能打开。
        assert_eq!(probe_system_source_dimensions(&tiff).unwrap(), (12_001, 1));
        let cache_dir = directory.path().join("cache");
        let decoded = decode_system_image_in(&tiff, Some(&cache_dir)).unwrap();
        assert!(decoded.width() <= SYSTEM_OVERSIZED_DECODE_MAX_SIDE);
        assert!(decoded.width() < 12_001);
        // 降采样后仍需向上层报告源图真实尺寸（走解码缓存，不再起子进程）。
        assert_eq!(probe_system_dimensions(&tiff).unwrap(), (12_001, 1));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn system_decodes_tiff_variants_and_probes_header() {
        for compression in ["raw", "tiff_lzw", "tiff_adobe_deflate"] {
            let directory = tempfile::tempdir().unwrap();
            let tiff = directory.path().join(format!("{compression}.tiff"));
            write_compressed_tiff(&tiff, compression);
            assert_eq!(probe_system_image(&tiff).unwrap(), (8, 6));
            let decoded = decode_system_image(&tiff).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (8, 6));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn raw_preview_path_uses_system_decoder() {
        let directory = tempfile::tempdir().unwrap();
        let raw = directory.path().join("preview.dng");
        write_compressed_tiff(&raw, "raw");
        assert!(is_raw(&raw));
        let decoded = decode_system_image(&raw).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (8, 6));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn colorsync_profile_conversion_changes_p3_pixel() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.png");
        let tagged = directory.path().join("tagged.png");
        let image: ImageBuffer<Rgb<u8>, _> = ImageBuffer::from_pixel(1, 1, Rgb([255, 80, 0]));
        DynamicImage::ImageRgb8(image).save(&source).unwrap();
        let output = Command::new("sips")
            .args(["-e", "/System/Library/ColorSync/Profiles/Display P3.icc"])
            .arg(&source)
            .args(["--out"])
            .arg(&tagged)
            .output()
            .unwrap();
        assert!(output.status.success());
        let before = image::open(&tagged).unwrap().get_pixel(0, 0);
        let after = decode_system_image(&tagged).unwrap().get_pixel(0, 0);
        assert_ne!(before, after);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn benchmark_system_tiff_decode() {
        let directory = tempfile::tempdir().unwrap();
        let png = directory.path().join("source.png");
        let tiff = directory.path().join("source.tiff");
        DynamicImage::new_rgb8(3000, 2000).save(&png).unwrap();
        assert!(Command::new("sips")
            .args(["-s", "format", "tiff"])
            .arg(&png)
            .args(["--out"])
            .arg(&tiff)
            .output()
            .unwrap()
            .status
            .success());
        let start = Instant::now();
        let decoded = decode_system_image(&tiff).unwrap();
        println!(
            "TIFF ImageIO/ColorSync decode {}×{}: {}ms",
            decoded.width(),
            decoded.height(),
            start.elapsed().as_millis()
        );
    }
}
