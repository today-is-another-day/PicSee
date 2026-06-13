use picsee_lib::images::{
    collect_image_entries, extract_open_paths, is_supported_image, natural_compare,
    scan_directory_entries,
};
use std::{
    cmp::Ordering,
    fs,
    path::{Path, PathBuf},
};

#[test]
fn supported_image_filter_is_case_insensitive_and_rejects_other_files() {
    for name in [
        "photo.jpg",
        "photo.JPEG",
        "photo.PnG",
        "photo.webp",
        "photo.gif",
        "photo.bmp",
        "photo.svg",
    ] {
        assert!(is_supported_image(Path::new(name)), "{name} 应被识别为图片");
    }

    for name in ["photo.txt", "photo.avif", "photo", ".jpg"] {
        assert!(
            !is_supported_image(Path::new(name)),
            "{name} 不应被识别为图片"
        );
    }
}

#[test]
fn argv_path_extraction_keeps_supported_existing_paths() {
    let directory = test_directory("argv");
    std::fs::create_dir_all(&directory).expect("应创建测试目录");
    let image = directory.join("open.png");
    let ignored = directory.join("ignored.txt");
    std::fs::write(&image, b"png").unwrap();
    std::fs::write(&ignored, b"text").unwrap();

    let paths = extract_open_paths([
        "picsee".to_string(),
        image.to_string_lossy().into_owned(),
        ignored.to_string_lossy().into_owned(),
    ]);

    assert_eq!(paths, vec![image.to_string_lossy().into_owned()]);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn natural_compare_orders_numeric_segments_by_value() {
    assert_eq!(natural_compare("img2.png", "img10.png"), Ordering::Less);
    assert_eq!(natural_compare("IMG10.png", "img2.png"), Ordering::Greater);
    assert_eq!(natural_compare("img02.png", "img2.png"), Ordering::Greater);
}

#[test]
fn scan_directory_returns_image_metadata_in_natural_order() {
    let directory = test_directory("scan");
    fs::create_dir_all(directory.join("nested")).expect("应创建测试目录");
    fs::write(directory.join("img10.png"), b"0123456789").expect("应写入测试图片");
    fs::write(directory.join("img2.JPG"), b"12").expect("应写入测试图片");
    fs::write(directory.join("notes.txt"), b"ignored").expect("应写入非图片文件");
    fs::write(directory.join("nested").join("img1.png"), b"ignored").expect("应写入嵌套图片");

    let result = scan_directory_entries(&directory).expect("目录扫描应成功");

    assert_eq!(result.directory, directory.to_string_lossy());
    assert_eq!(result.entries.len(), 2);
    assert_eq!(result.entries[0].name, "img2.JPG");
    assert_eq!(result.entries[0].size, 2);
    assert!(result.entries[0].modified > 0);
    assert_eq!(result.entries[1].name, "img10.png");
    assert_eq!(result.entries[1].size, 10);
    assert_eq!(
        result.entries[0].path,
        directory.join("img2.JPG").to_string_lossy()
    );

    let value = serde_json::to_value(&result).expect("目录扫描结果应可序列化");
    assert!(value["entries"].is_array());
    assert!(value.get("images").is_none());
    let entry = &value["entries"][0];
    assert!(entry["path"].is_string());
    assert!(entry["name"].is_string());
    assert!(entry["size"].is_number());
    assert!(entry["modified"].is_number());

    fs::remove_dir_all(directory).expect("应清理测试目录");
}

#[test]
fn scan_skips_an_image_that_disappears_during_metadata_read() {
    let directory = test_directory("skip-failed-entry");
    fs::create_dir_all(&directory).expect("应创建测试目录");
    let valid = directory.join("valid.png");
    let disappeared = directory.join("disappeared.png");
    fs::write(&valid, b"valid").expect("应写入有效图片");

    let entries = collect_image_entries(vec![disappeared, valid]).expect("单文件失败不应中断扫描");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "valid.png");
    fs::remove_dir_all(directory).expect("应清理测试目录");
}

fn test_directory(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-data")
        .join(format!("picsee-images-{name}-{}", std::process::id()))
}
