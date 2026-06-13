pub mod images;
pub mod large_image;
pub mod settings;
pub mod thumbnails;

use images::{open_directory, open_image_file, scan_directory};
use large_image::policy::probe_image;
use large_image::session::{close_large_image, open_large_image, LargeImageState};
use settings::{get_settings, read_settings_file, save_settings};
use std::sync::{Arc, Mutex};
use tauri::Manager;
use thumbnails::{clear_thumbnail_cache, get_thumbnail, ThumbnailState};

/// 从 picsee:// URL 中提取路径部分（不含 query string）。
fn extract_picsee_path(url: &str) -> &str {
    if let Some(rest) = url.strip_prefix("picsee://localhost") {
        rest.split('?').next().unwrap_or(rest)
    } else if let Some(idx) = url.find("//") {
        let after_scheme = &url[idx + 2..];
        if let Some(slash_idx) = after_scheme.find('/') {
            &after_scheme[slash_idx..]
        } else {
            "/"
        }
    } else {
        "/"
    }
}

/// 解析整数路径段。
fn parse_u32(s: &str) -> Option<u32> {
    s.parse().ok()
}

fn parse_u64(s: &str) -> Option<u64> {
    s.parse().ok()
}

fn image_response(data: Vec<u8>, cache_control: &str) -> tauri::http::Response<Vec<u8>> {
    tauri::http::Response::builder()
        .status(200)
        .header("Content-Type", "image/webp")
        .header("Cache-Control", cache_control)
        .body(data)
        .unwrap()
}

fn error_response(
    status: u16,
    err: large_image::LargeImageError,
) -> tauri::http::Response<Vec<u8>> {
    tauri::http::Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(serde_json::to_vec(&err).unwrap_or_default())
        .unwrap()
}

fn not_found_response() -> tauri::http::Response<Vec<u8>> {
    tauri::http::Response::builder()
        .status(404)
        .body(b"Not found".to_vec())
        .unwrap()
}

/// Build and run the PicSee Tauri application.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .register_asynchronous_uri_scheme_protocol("picsee", {
            move |ctx, request, responder| {
                let url = request.uri().to_string();
                let path = extract_picsee_path(&url);
                let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

                let state_arc: Arc<Mutex<LargeImageState>> = ctx
                    .app_handle()
                    .state::<Arc<Mutex<LargeImageState>>>()
                    .inner()
                    .clone();

                // /preview/{session_id}
                if segments.first() == Some(&"preview") && segments.len() == 2 {
                    if let Some(session_id) = parse_u64(segments[1]) {
                        let response = match large_image::session::handle_preview_request(
                            &state_arc, session_id,
                        ) {
                            Ok(data) => image_response(data, "no-store"),
                            Err((status, err)) => error_response(status, err),
                        };
                        responder.respond(response);
                        return;
                    }
                }

                // /tile/{session_id}/{z}/{tx}/{ty}
                if segments.first() == Some(&"tile") && segments.len() == 5 {
                    if let (Some(session_id), Some(z), Some(tx), Some(ty)) = (
                        parse_u64(segments[1]),
                        parse_u32(segments[2]),
                        parse_u32(segments[3]),
                        parse_u32(segments[4]),
                    ) {
                        let semaphore = state_arc.lock().unwrap().semaphore.clone();
                        tauri::async_runtime::spawn(async move {
                            let response = match semaphore.acquire_owned().await {
                                Ok(_permit) => {
                                    match tauri::async_runtime::spawn_blocking(move || {
                                        large_image::session::handle_tile_request(
                                            state_arc, session_id, z, tx, ty,
                                        )
                                    })
                                    .await
                                    {
                                        Ok(Ok(data)) => image_response(data, "max-age=3600"),
                                        Ok(Err((status, err))) => error_response(status, err),
                                        Err(err) => error_response(
                                            500,
                                            large_image::LargeImageError::io(format!(
                                                "tile task failed: {err}"
                                            )),
                                        ),
                                    }
                                }
                                Err(err) => error_response(
                                    500,
                                    large_image::LargeImageError::io(format!(
                                        "tile semaphore closed: {err}"
                                    )),
                                ),
                            };
                            responder.respond(response);
                        });
                        return;
                    }
                }

                responder.respond(not_found_response());
            }
        })
        .setup(|app| {
            // 读取启动设置
            let settings_path: Option<std::path::PathBuf> = app
                .path()
                .app_config_dir()
                .map(|d: std::path::PathBuf| d.join("settings.json"))
                .ok();
            let settings = settings_path
                .as_deref()
                .and_then(|p| read_settings_file(p).ok())
                .unwrap_or_default();

            // M3: 缩略图并发数
            let concurrency = settings.performance.thumbnail_concurrency.clamp(1, 16);
            app.manage(ThumbnailState::new(concurrency));

            // M4: 大图 managed state
            let tile_concurrency = settings.performance.tile_concurrency.clamp(1, 16) as usize;
            let memory_limit_mb = settings.cache.memory_cache_limit_mb as usize;
            app.manage(Arc::new(Mutex::new(LargeImageState::new(
                tile_concurrency,
                memory_limit_mb,
            ))));

            // 授权缩略图缓存目录
            if let Ok(cache_dir) = app.path().app_cache_dir() {
                let thumb_dir: std::path::PathBuf = cache_dir.join("thumbnails");
                let _ = app
                    .asset_protocol_scope()
                    .allow_directory(&thumb_dir, false);
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
            probe_image,
            open_large_image,
            close_large_image,
        ])
        .run(tauri::generate_context!())
        .expect("Error running PicSee");
}

#[cfg(test)]
mod tests {
    use super::extract_picsee_path;

    #[test]
    fn test_extract_picsee_path_localhost() {
        assert_eq!(
            extract_picsee_path("picsee://localhost/preview/1"),
            "/preview/1"
        );
    }

    #[test]
    fn test_extract_picsee_path_strips_query() {
        assert_eq!(
            extract_picsee_path("picsee://localhost/tile/1/0/2/3?foo=bar"),
            "/tile/1/0/2/3"
        );
    }
}
