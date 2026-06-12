pub mod images;
pub mod settings;
pub mod thumbnails;

use images::{open_directory, open_image_file, scan_directory};
use settings::{get_settings, save_settings, read_settings_file};
use thumbnails::{clear_thumbnail_cache, get_thumbnail, ThumbnailState};
use tauri::Manager;

/// Build and run the PicSee Tauri application.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // M3: read thumbnail concurrency from persisted settings at startup.
            // Changes to this setting take effect on the next app launch.
            let concurrency = {
                let settings_path: Option<std::path::PathBuf> = app
                    .path()
                    .app_config_dir()
                    .map(|d: std::path::PathBuf| d.join("settings.json"))
                    .ok();
                let settings = settings_path
                    .as_deref()
                    .and_then(|p| read_settings_file(p).ok())
                    .unwrap_or_default();
                // Clamp to a sane range [1, 16].
                settings.performance.thumbnail_concurrency.clamp(1, 16)
            };
            app.manage(ThumbnailState::new(concurrency));

            // Minor 2: grant asset scope for the thumbnail cache directory once at startup,
            // eliminating repeated per-request authorization calls in the hot path.
            if let Ok(cache_dir) = app.path().app_cache_dir() {
                let thumb_dir: std::path::PathBuf = cache_dir.join("thumbnails");
                // Directory may not exist yet; allow_directory creates scope entry regardless.
                let _ = app.asset_protocol_scope().allow_directory(&thumb_dir, false);
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            open_image_file,
            open_directory,
            scan_directory,
            get_thumbnail,
            clear_thumbnail_cache,
        ])
        .run(tauri::generate_context!())
        .expect("Error running PicSee");
}
