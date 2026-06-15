pub mod color;
pub mod extended_formats;
pub mod file_operations;
pub mod images;
pub mod large_image;
pub mod settings;
pub mod thumbnails;

use extended_formats::prefetch_system_decode;
use file_operations::{copy_file_to_clipboard, move_to_trash, reveal_in_finder};
use images::{open_directory, open_external_path, open_image_file, scan_directory};
use large_image::policy::probe_image;
use large_image::session::{close_large_image, get_preview, open_large_image, LargeImageState};
use settings::{get_settings, read_settings_file, save_settings};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};
use thumbnails::{clear_thumbnail_cache, enforce_disk_cache_limit, get_thumbnail, ThumbnailState};

#[derive(Default)]
struct PendingState {
    paths: Vec<String>,
    /// 前端是否已注册 open-paths 监听并就绪。
    frontend_ready: bool,
}

#[derive(Default)]
struct PendingOpenPaths(Mutex<PendingState>);

impl PendingOpenPaths {
    fn new(paths: Vec<String>) -> Self {
        Self(Mutex::new(PendingState {
            paths,
            frontend_ready: false,
        }))
    }

    /// 前端就绪：原子地置就绪并取走累积的待打开路径。
    fn mark_ready_and_take(&self) -> Vec<String> {
        let mut state = self.0.lock().unwrap();
        state.frontend_ready = true;
        std::mem::take(&mut state.paths)
    }

    /// 运行期/启动期打开请求:前端未就绪则入队并返回 false(调用方不要 emit,
    /// 等前端就绪时由 mark_ready_and_take 取走);已就绪返回 true(走事件通道 emit)。
    fn enqueue_or_ready(&self, paths: &[String]) -> bool {
        let mut state = self.0.lock().unwrap();
        if state.frontend_ready {
            true
        } else {
            state.paths.extend_from_slice(paths);
            false
        }
    }
}

#[tauri::command]
fn take_pending_open_paths(state: tauri::State<'_, PendingOpenPaths>) -> Vec<String> {
    state.mark_ready_and_take()
}

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

                // /preview/{session_id} —— 导航窗小 WebP（主画布预览走 get_preview 命令）
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
            app.manage(PendingOpenPaths::new(images::extract_open_paths(
                std::env::args(),
            )));

            // 授权缩略图缓存目录
            if let Ok(cache_dir) = app.path().app_cache_dir() {
                let thumb_dir: std::path::PathBuf = cache_dir.join("thumbnails");
                let _ = app
                    .asset_protocol_scope()
                    .allow_directory(&thumb_dir, false);
                // 清理上次遗留的大图临时栅格（非 BMP 大图分块用）
                let _ = std::fs::remove_dir_all(cache_dir.join("large-raster"));
                // 内存缓存重启后为空，清理上次遗留的系统解码 PNG。
                let _ = std::fs::remove_dir_all(cache_dir.join("system-decode"));
                // 启动后在后台按磁盘缓存水位淘汰旧缩略图，不阻塞应用初始化。
                let disk_cache_limit_bytes = settings
                    .cache
                    .disk_cache_limit_mb
                    .saturating_mul(1024 * 1024);
                tauri::async_runtime::spawn_blocking(move || {
                    enforce_disk_cache_limit(&thumb_dir, disk_cache_limit_bytes)
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            open_image_file,
            open_directory,
            scan_directory,
            open_external_path,
            move_to_trash,
            reveal_in_finder,
            copy_file_to_clipboard,
            get_thumbnail,
            clear_thumbnail_cache,
            probe_image,
            prefetch_system_decode,
            open_large_image,
            close_large_image,
            get_preview,
            take_pending_open_paths,
        ])
        .build(tauri::generate_context!())
        .expect("Error building PicSee")
        .run(|app, event| {
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Opened { urls } = event {
                let paths: Vec<String> = urls
                    .into_iter()
                    .filter_map(|url| url.to_file_path().ok())
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect();
                if !paths.is_empty() {
                    // 前端就绪 → 走事件通道；未就绪(冷启动 Apple Event 早于前端监听)→
                    // 入队,待前端 take_pending_open_paths 时取走,避免首次双击丢失。
                    if app.state::<PendingOpenPaths>().enqueue_or_ready(&paths) {
                        let _ = app.emit("open-paths", paths);
                    }
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::{extract_picsee_path, PendingOpenPaths};

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

    #[test]
    fn pending_argv_paths_are_consumed_once() {
        let pending = PendingOpenPaths::new(vec!["/tmp/image.png".to_string()]);

        assert_eq!(pending.mark_ready_and_take(), vec!["/tmp/image.png"]);
        assert!(pending.mark_ready_and_take().is_empty());
    }

    #[test]
    fn opened_paths_queue_until_frontend_ready() {
        let pending = PendingOpenPaths::new(Vec::new());

        // 前端未就绪：Apple Event 入队、不 emit。
        assert!(!pending.enqueue_or_ready(&["/tmp/a.png".to_string()]));
        assert!(!pending.enqueue_or_ready(&["/tmp/b.png".to_string()]));

        // 前端就绪：取走累积的全部队列。
        assert_eq!(
            pending.mark_ready_and_take(),
            vec!["/tmp/a.png".to_string(), "/tmp/b.png".to_string()]
        );

        // 就绪后:走事件通道(返回 true),不再入队。
        assert!(pending.enqueue_or_ready(&["/tmp/c.png".to_string()]));
        assert!(pending.mark_ready_and_take().is_empty());
    }
}
