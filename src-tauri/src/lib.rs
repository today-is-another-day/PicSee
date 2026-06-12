pub mod settings;

use settings::{get_settings, save_settings};

/// 构建并运行 PicSee Tauri 应用。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![get_settings, save_settings])
        .run(tauri::generate_context!())
        .expect("运行 PicSee 时发生错误");
}
