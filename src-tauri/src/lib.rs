pub mod images;
pub mod settings;
pub mod thumbnails;

use images::{open_directory, open_image_file, scan_directory};
use settings::{get_settings, save_settings};
use thumbnails::{clear_thumbnail_cache, get_thumbnail, ThumbnailState};

/// 构建并运行 PicSee Tauri 应用。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(ThumbnailState::new(4))
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
        .expect("运行 PicSee 时发生错误");
}
