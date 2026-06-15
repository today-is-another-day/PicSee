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
const SIPS_TIMEOUT: Duration = Duration::from_secs(30);
const SYSTEM_DECODE_CACHE_CAPACITY: usize = 6;
pub const SYSTEM_MAX_SIDE_PIXELS: u32 = 12_000;
pub const SYSTEM_MAX_DECODE_BYTES: u64 = 512 * 1024 * 1024;

type CacheKey = (PathBuf, i64);

#[derive(Clone)]
struct CachedDecode {
    width: u32,
    height: u32,
    png_path: PathBuf,
}

pub const TIFF_EXTENSIONS: [&str; 2] = ["tif", "tiff"];
pub const SYSTEM_EXTENSIONS: [&str; 10] = [
    "heic", "heif", "dng", "cr2", "cr3", "nef", "arw", "raf", "orf", "rw2",
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

/// 使用 macOS ImageIO/ColorSync 解码为 PNG，再交给 image-rs 消费。
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

    // 未命中缓存的入口先做 header-only 安全检查，避免未来新增调用点绕过尺寸限制。
    probe_system_image(path)?;
    let directory = runtime_decode_directory(preferred_directory)?;
    let temporary_output = temporary_png_path(&directory);
    let cached_output = cache_key
        .as_ref()
        .map(|key| cached_png_path(&directory, key));
    let mut command = Command::new("sips");
    command
        .args(["-s", "format", "png"])
        .args(["-m", "/System/Library/ColorSync/Profiles/sRGB Profile.icc"])
        .arg(path)
        .args(["--out"])
        .arg(&temporary_output);

    record_system_decode(path);
    let result = run_command_with_timeout(&mut command, SIPS_TIMEOUT);
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
                            width: decoded.width(),
                            height: decoded.height(),
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
                "macOS ImageIO 无法解码此格式: {}",
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

/// 用 sips 子进程把任意大图降采样为最长边 ≤ `max_side` 的临时 PNG，再交给 image-rs。
///
/// 关键价值：解码在 sips 进程内完成（内存隔离），本进程只读取降采样后的小图，
/// 避免对超大图（> 1 亿像素 / 超长边）在本进程整图解码导致 OOM。
pub fn downscale_with_sips_in(
    path: &Path,
    max_side: u32,
    preferred_directory: Option<&Path>,
) -> Result<DynamicImage, String> {
    let directory = runtime_decode_directory(preferred_directory)?;
    let output = temporary_png_path(&directory);
    let mut command = Command::new("sips");
    command
        .args(["-s", "format", "png"])
        .args(["-Z", &max_side.to_string()])
        .arg(path)
        .args(["--out"])
        .arg(&output);

    let result = run_command_with_timeout(&mut command, SIPS_TIMEOUT);
    let decoded = match result {
        Ok(result) if result.status.success() => {
            image::open(&output).map_err(|error| format!("读取降采样 PNG 失败: {error}"))
        }
        Ok(result) => Err(format!(
            "sips 降采样失败: {}",
            String::from_utf8_lossy(&result.stderr)
        )),
        Err(error) => Err(error),
    };
    let _ = std::fs::remove_file(&output);
    decoded
}

/// 仅通过 sips 元数据读取尺寸，不生成临时 PNG、不全量解码。
pub fn probe_system_image(path: &Path) -> Result<(u32, u32), String> {
    let mut command = Command::new("sips");
    command
        .args(["-g", "pixelWidth", "-g", "pixelHeight"])
        .arg(path);
    record_system_probe(path);
    let output = run_command_with_timeout(&mut command, SIPS_TIMEOUT)?;
    if !output.status.success() {
        return Err(format!(
            "macOS ImageIO 无法读取图像尺寸: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let width = parse_sips_property(&stdout, "pixelWidth")?;
    let height = parse_sips_property(&stdout, "pixelHeight")?;
    validate_system_dimensions(width, height)?;
    Ok((width, height))
}

/// 优先复用系统解码缓存中的安全尺寸；未命中时才调用 sips 元数据探测。
pub fn probe_system_dimensions(path: &Path) -> Result<(u32, u32), String> {
    if let Some(cached) = system_decode_cache_key(path)
        .as_ref()
        .and_then(get_cached_decode)
    {
        return Ok((cached.width, cached.height));
    }
    probe_system_image(path)
}

/// 判断当前文件版本是否已有可读的系统解码缓存。
pub fn is_system_decode_cached(path: &Path) -> bool {
    system_decode_cache_key(path)
        .as_ref()
        .and_then(get_cached_decode)
        .is_some()
}

/// 顺序预解码邻图；全局单通道避免快速切图时同时启动多个 sips 子进程。
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

    for path in paths {
        let path = PathBuf::from(path);
        if !is_system_decoded(&path) || is_system_decode_cached(&path) {
            continue;
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

fn parse_sips_property(output: &str, property: &str) -> Result<u32, String> {
    output
        .lines()
        .find_map(|line| {
            let (key, value) = line.trim().split_once(':')?;
            (key == property).then(|| value.trim().parse::<u32>().ok())?
        })
        .ok_or_else(|| format!("sips 输出缺少 {property}"))
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

#[cfg(test)]
fn system_decode_count(path: &Path) -> u64 {
    test_command_counts()
        .lock()
        .unwrap()
        .get(&(path.to_path_buf(), "decode"))
        .copied()
        .unwrap_or_default()
}

#[cfg(test)]
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
        .map_err(|error| format!("无法启动 macOS ImageIO 解码: {error}"))?;
    let started = Instant::now();
    loop {
        match child
            .try_wait()
            .map_err(|error| format!("等待 macOS ImageIO 解码失败: {error}"))?
        {
            Some(_) => {
                return child
                    .wait_with_output()
                    .map_err(|error| format!("读取 macOS ImageIO 输出失败: {error}"));
            }
            None if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "macOS ImageIO 解码超时（{} 秒）",
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
    use image::{GenericImageView, ImageBuffer, Rgb};
    use std::io::Write;

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
                width: 8,
                height: 6,
                png_path: first_png.clone(),
            },
        );
        insert_cached_decode(
            &mut cache,
            (directory.path().join("second.tiff"), 2),
            CachedDecode {
                width: 8,
                height: 6,
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

    #[test]
    fn system_probe_rejects_oversized_tiff_before_decode() {
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
    }

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

    #[test]
    fn raw_preview_path_uses_system_decoder() {
        let directory = tempfile::tempdir().unwrap();
        let raw = directory.path().join("preview.dng");
        write_compressed_tiff(&raw, "raw");
        assert!(is_raw(&raw));
        let decoded = decode_system_image(&raw).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (8, 6));
    }

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
