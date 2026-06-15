use picsee_lib::settings::{
    AppSettings, DefaultZoomMode, Language, NavigatorMode, NavigatorSize, PreviewMaxSize, Theme,
    ThumbnailPosition, TileSize, ViewerBackground,
};

#[test]
fn default_settings_match_expected_baseline() {
    let settings = AppSettings::default();

    assert_eq!(settings.language, Language::System);
    assert_eq!(settings.theme, Theme::System);
    assert!(settings.shortcuts.is_empty());
    assert_eq!(
        settings.viewer.default_zoom_mode,
        DefaultZoomMode::FitWindow
    );
    assert_eq!(settings.viewer.navigator_mode, NavigatorMode::Auto);
    assert_eq!(settings.viewer.navigator_size, NavigatorSize::Size200);
    assert!(!settings.viewer.confirm_delete);
    assert_eq!(settings.viewer.viewer_background, ViewerBackground::Dark);
    assert_eq!(settings.viewer.viewer_background_color, "#202020");
    assert_eq!(
        settings.large_image.preview_max_size,
        PreviewMaxSize::Size4096
    );
    assert_eq!(settings.large_image.tile_size, TileSize::Size512);
    assert_eq!(settings.large_image.file_size_threshold_mb, 300);
    assert_eq!(settings.large_image.pixel_threshold, 50_000_000);
    assert_eq!(settings.large_image.side_threshold, 12_000);
    assert_eq!(settings.large_image.pyramid_disk_cache_mb, 1024);
    assert_eq!(settings.large_image.neighbor_prefetch_count, 1);
    assert_eq!(
        settings.layout.thumbnail_position,
        ThumbnailPosition::Bottom
    );
}

#[test]
fn settings_serialize_with_camel_case_fields_and_enum_values() {
    let value = serde_json::to_value(AppSettings::default()).expect("设置应可序列化");

    assert_eq!(value["viewer"]["defaultZoomMode"], "fit-window");
    assert_eq!(value["viewer"]["viewerBackground"], "dark");
    assert_eq!(value["viewer"]["viewerBackgroundColor"], "#202020");
    assert!(value["largeImage"]["fileSizeThresholdMB"].is_number());
    assert!(value["largeImage"]["pyramidDiskCacheMB"].is_number());
    assert!(value["largeImage"]["neighborPrefetchCount"].is_number());
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
fn settings_with_missing_fields_use_defaults() {
    let settings: AppSettings = serde_json::from_str(
        r#"{
            "language": "en-US",
            "viewer": {
                "zoomStep": 0.25
            }
        }"#,
    )
    .expect("缺少新增字段的旧设置应可解析");

    assert_eq!(settings.language, Language::EnUs);
    assert_eq!(settings.theme, Theme::System);
    assert_eq!(settings.viewer.zoom_step, 0.25);
    assert!(settings.viewer.smooth_zoom);
    assert_eq!(settings.viewer.viewer_background, ViewerBackground::Dark);
    assert_eq!(settings.large_image, AppSettings::default().large_image);
}

#[test]
fn corrupt_settings_file_is_backed_up_and_defaults_are_returned() {
    let temp_dir = test_directory("corrupt-settings");
    let path = temp_dir.join("settings.json");
    let backup_path = temp_dir.join("settings.json.bak");
    std::fs::create_dir_all(&temp_dir).expect("应创建测试目录");
    std::fs::write(&path, "{ invalid json").expect("应写入损坏设置");

    let loaded = picsee_lib::settings::read_settings_file(&path).expect("损坏设置应回退默认值");

    assert_eq!(loaded, AppSettings::default());
    assert!(!path.exists());
    assert_eq!(
        std::fs::read_to_string(backup_path).expect("损坏设置应备份"),
        "{ invalid json"
    );
    std::fs::remove_dir_all(temp_dir).expect("应清理测试目录");
}

#[test]
fn settings_file_round_trip_preserves_values() {
    let temp_dir = test_directory("round-trip");
    let path = temp_dir.join("settings.json");
    let mut settings = AppSettings::default();
    settings.viewer.zoom_step = 0.25;
    settings.cache.enable_disk_cache = false;

    picsee_lib::settings::write_settings_file(&path, &settings).expect("设置应写入成功");
    let loaded = picsee_lib::settings::read_settings_file(&path).expect("设置应读取成功");

    assert_eq!(loaded, settings);
    std::fs::remove_dir_all(temp_dir).expect("应清理测试目录");
}

#[test]
fn shortcuts_round_trip_and_missing_field_default() {
    let mut settings = AppSettings::default();
    settings
        .shortcuts
        .insert("openFile".into(), "Mod+KeyP".into());
    let json = serde_json::to_string(&settings).expect("设置应可序列化");
    let loaded: AppSettings = serde_json::from_str(&json).expect("快捷键设置应可反序列化");
    assert_eq!(
        loaded.shortcuts.get("openFile"),
        Some(&"Mod+KeyP".to_string())
    );

    let mut legacy = serde_json::to_value(&settings).expect("设置应可序列化");
    legacy.as_object_mut().unwrap().remove("shortcuts");
    let loaded_legacy: AppSettings =
        serde_json::from_value(legacy).expect("缺少 shortcuts 的旧设置应可加载");
    assert!(loaded_legacy.shortcuts.is_empty());
}

fn test_directory(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-data")
        .join(format!("picsee-settings-{name}-{}", std::process::id()))
}
