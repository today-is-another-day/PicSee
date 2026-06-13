use std::{path::PathBuf, process::Command};

fn run_command(program: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| format!("启动 {program} 失败: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

/// 将文件移到 macOS 废纸篓。
#[tauri::command]
pub async fn move_to_trash(path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let canonical = PathBuf::from(path)
            .canonicalize()
            .map_err(|error| format!("无法读取待删除文件: {error}"))?;
        let script = format!(
            "tell application \"Finder\" to delete POSIX file {}",
            apple_script_string(&canonical.to_string_lossy())
        );
        run_command("osascript", &["-e", &script])
    })
    .await
    .map_err(|error| format!("移动到废纸篓任务失败: {error}"))?
}

/// 在 Finder 中定位文件。
#[tauri::command]
pub async fn reveal_in_finder(path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || run_command("open", &["-R", &path]))
        .await
        .map_err(|error| format!("Finder 定位任务失败: {error}"))?
}

/// 将文件引用复制到 macOS 剪贴板，可直接粘贴到 Finder。
#[tauri::command]
pub async fn copy_file_to_clipboard(path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let script = format!(
            "set the clipboard to POSIX file {}",
            apple_script_string(&path)
        );
        run_command("osascript", &["-e", &script])
    })
    .await
    .map_err(|error| format!("复制文件任务失败: {error}"))?
}

fn apple_script_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_to_trash_source_can_be_removed_from_tempdir() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("image.png");
        let trash = temp.path().join("trash");
        std::fs::create_dir(&trash).unwrap();
        std::fs::write(&source, b"image").unwrap();

        std::fs::rename(&source, trash.join("image.png")).unwrap();

        assert!(!source.exists());
        assert!(trash.join("image.png").exists());
    }

    #[test]
    fn apple_script_string_escapes_quotes() {
        assert_eq!(apple_script_string("a\"b"), "\"a\\\"b\"");
    }
}
