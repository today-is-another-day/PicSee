use image::DynamicImage;
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(1);

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

pub fn is_system_decoded(path: &Path) -> bool {
    is_tiff(path) || SYSTEM_EXTENSIONS.contains(&extension(path).as_str())
}

pub fn is_raw(path: &Path) -> bool {
    RAW_EXTENSIONS.contains(&extension(path).as_str())
}

/// 后端转码输出需要统一匹配到 sRGB 的格式。
///
/// 普通 JPG/PNG 直接由 WKWebView 显示时由 WebKit 处理 ICC；只有进入后端
/// preview/thumbnail 链路时才通过 ColorSync 转换。
pub fn needs_colorsync_output(path: &Path) -> bool {
    is_system_decoded(path) || ["jpg", "jpeg", "png"].contains(&extension(path).as_str())
}

/// 使用 macOS ImageIO/ColorSync 解码为 PNG，再交给 image-rs 消费。
///
/// TIFF、HEIC 和 RAW embedded preview 均走这条路径；ColorSync 会在输出 PNG 时转换色彩。
pub fn decode_system_image(path: &Path) -> Result<DynamicImage, String> {
    let output = temporary_png_path(path)?;
    let result = Command::new("sips")
        .args(["-s", "format", "png"])
        .args(["-m", "/System/Library/ColorSync/Profiles/sRGB Profile.icc"])
        .arg(path)
        .args(["--out"])
        .arg(&output)
        .output()
        .map_err(|error| format!("无法启动 macOS ImageIO 解码: {error}"))?;

    if !result.status.success() {
        let _ = std::fs::remove_file(&output);
        return Err(format!(
            "macOS ImageIO 无法解码此格式: {}",
            String::from_utf8_lossy(&result.stderr)
        ));
    }
    let decoded = image::open(&output).map_err(|error| format!("读取系统解码 PNG 失败: {error}"));
    let _ = std::fs::remove_file(output);
    decoded
}

pub fn probe_system_image(path: &Path) -> Result<(u32, u32), String> {
    let image = decode_system_image(path)?;
    Ok((image.width(), image.height()))
}

fn temporary_png_path(source: &Path) -> Result<PathBuf, String> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("picsee-system-decode");
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("创建系统解码临时目录失败: {error}"))?;
    let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("image");
    Ok(directory.join(format!("{stem}-{}-{id}.png", std::process::id())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GenericImageView, ImageBuffer, Rgb};

    fn test_directory(name: &str) -> PathBuf {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("test-data")
            .join(format!("picsee-m6-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

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
    fn raw_extension_classification() {
        assert!(is_raw(Path::new("sample.cr3")));
        assert!(is_raw(Path::new("sample.NEF")));
        assert!(!is_raw(Path::new("sample.heic")));
    }

    #[test]
    fn tiff_extension_classification() {
        assert!(is_tiff(Path::new("sample.tiff")));
        assert!(is_tiff(Path::new("sample.TIF")));
    }

    #[test]
    fn backend_jpeg_and_png_outputs_use_colorsync() {
        assert!(needs_colorsync_output(Path::new("sample.jpg")));
        assert!(needs_colorsync_output(Path::new("sample.png")));
        assert!(!needs_colorsync_output(Path::new("sample.gif")));
    }

    #[test]
    fn system_tiff_decode_round_trip() {
        let directory = test_directory("tiff");
        let png = directory.join("source.png");
        let tiff = directory.join("source.tiff");
        DynamicImage::new_rgb8(4, 3).save(&png).unwrap();
        let output = Command::new("sips")
            .args(["-s", "format", "tiff"])
            .arg(&png)
            .args(["--out"])
            .arg(&tiff)
            .output()
            .unwrap();
        assert!(output.status.success());
        let decoded = decode_system_image(&tiff).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (4, 3));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn system_decodes_lzw_tiff() {
        let directory = test_directory("tiff-lzw");
        let tiff = directory.join("lzw.tiff");
        write_compressed_tiff(&tiff, "tiff_lzw");
        let decoded = decode_system_image(&tiff).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (8, 6));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn system_decodes_deflate_tiff() {
        let directory = test_directory("tiff-deflate");
        let tiff = directory.join("deflate.tiff");
        write_compressed_tiff(&tiff, "tiff_adobe_deflate");
        let decoded = decode_system_image(&tiff).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (8, 6));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn raw_preview_path_uses_system_decoder() {
        let directory = test_directory("raw-preview");
        let raw = directory.join("preview.dng");
        write_compressed_tiff(&raw, "raw");
        assert!(is_raw(&raw));
        let decoded = decode_system_image(&raw).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (8, 6));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn colorsync_profile_conversion_changes_p3_pixel() {
        let directory = test_directory("icc");
        let source = directory.join("source.png");
        let tagged = directory.join("tagged.png");
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
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    #[ignore]
    fn benchmark_system_tiff_decode() {
        let directory = test_directory("benchmark-tiff");
        let png = directory.join("source.png");
        let tiff = directory.join("source.tiff");
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
        let start = std::time::Instant::now();
        let decoded = decode_system_image(&tiff).unwrap();
        println!(
            "TIFF ImageIO/ColorSync decode {}×{}: {}ms",
            decoded.width(),
            decoded.height(),
            start.elapsed().as_millis()
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
