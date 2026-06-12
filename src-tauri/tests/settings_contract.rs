use picsee_lib::settings::{
    AppSettings, DefaultZoomMode, Language, PreviewMaxSize, Theme, ThumbnailPosition, TileSize,
};

#[test]
fn default_settings_match_expected_baseline() {
    let settings = AppSettings::default();

    assert_eq!(settings.language, Language::System);
    assert_eq!(settings.theme, Theme::System);
    assert_eq!(
        settings.viewer.default_zoom_mode,
        DefaultZoomMode::FitWindow
    );
    assert_eq!(
        settings.large_image.preview_max_size,
        PreviewMaxSize::Size4096
    );
    assert_eq!(settings.large_image.tile_size, TileSize::Size512);
    assert_eq!(settings.large_image.file_size_threshold_mb, 300);
    assert_eq!(settings.large_image.pixel_threshold, 50_000_000);
    assert_eq!(settings.large_image.side_threshold, 12_000);
    assert_eq!(
        settings.layout.thumbnail_position,
        ThumbnailPosition::Bottom
    );
}

#[test]
fn settings_serialize_with_camel_case_fields_and_enum_values() {
    let value = serde_json::to_value(AppSettings::default()).expect("设置应可序列化");

    assert_eq!(value["viewer"]["defaultZoomMode"], "fit-window");
    assert!(value["largeImage"]["fileSizeThresholdMB"].is_number());
    assert!(value["cache"]["memoryCacheLimitMB"].is_number());
    assert!(value["cache"]["diskCacheLimitMB"].is_number());
    assert!(value["cache"]["clearTempTileOnExit"].is_boolean());
    assert!(value["performance"]["tileConcurrency"].is_number());
    assert!(value["layout"]["showThumbnailBar"].is_boolean());
}

#[test]
fn settings_reject_unsupported_limited_numeric_values() {
    let mut value = serde_json::to_value(AppSettings::default()).expect("设置应可序列化");
    value["largeImage"]["previewMaxSize"] = 1024.into();
    value["largeImage"]["tileSize"] = 128.into();

    assert!(serde_json::from_value::<AppSettings>(value).is_err());
}

#[test]
fn settings_file_round_trip_preserves_values() {
    let temp_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-data")
        .join(format!("picsee-settings-test-{}", std::process::id()));
    let path = temp_dir.join("settings.json");
    let mut settings = AppSettings::default();
    settings.viewer.zoom_step = 0.25;
    settings.cache.enable_disk_cache = false;

    picsee_lib::settings::write_settings_file(&path, &settings).expect("设置应写入成功");
    let loaded = picsee_lib::settings::read_settings_file(&path).expect("设置应读取成功");

    assert_eq!(loaded, settings);
    std::fs::remove_dir_all(temp_dir).expect("应清理测试目录");
}
