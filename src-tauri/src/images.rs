use serde::Serialize;
use std::{
    cmp::Ordering,
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;

const IMAGE_EXTENSIONS: [&str; 20] = [
    "jpg", "jpeg", "png", "webp", "gif", "bmp", "svg", "tiff", "tif", "heic", "heif", "dng", "cr2",
    "cr3", "nef", "arw", "raf", "orf", "rw2", "pef",
];

/// 图片文件信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageEntry {
    /// 图片绝对或调用方提供的路径。
    pub path: String,
    /// 图片文件名。
    pub name: String,
    /// 文件大小，单位为字节。
    pub size: u64,
    /// 最后修改时间，Unix 时间戳，单位为毫秒。
    pub modified: u64,
}

/// 目录扫描结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryScan {
    /// 已扫描目录。
    pub directory: String,
    /// 目录当前层级中的图片。
    pub entries: Vec<ImageEntry>,
}

/// 打开文件选择对话框并返回所选图片。
#[tauri::command]
pub async fn open_image_file(app: AppHandle) -> Result<Option<ImageEntry>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let selected = app
            .dialog()
            .file()
            .add_filter("图片", &IMAGE_EXTENSIONS)
            .blocking_pick_file();
        let Some(selected) = selected else {
            return Ok(None);
        };
        let path = selected
            .into_path()
            .map_err(|error| format!("无法读取所选图片路径: {error}"))?;

        if !is_supported_image(&path) {
            return Err(format!("不支持的图片格式: {}", path.display()));
        }
        let parent = path
            .parent()
            .ok_or_else(|| format!("无法确定图片父目录: {}", path.display()))?;
        app.asset_protocol_scope()
            .allow_directory(parent, false)
            .map_err(|error| format!("授权图片父目录 {} 失败: {error}", parent.display()))?;

        image_entry(&path).map(Some)
    })
    .await
    .map_err(|error| format!("打开图片任务失败: {error}"))?
}

/// 打开目录选择对话框并扫描所选目录。
#[tauri::command]
pub async fn open_directory(app: AppHandle) -> Result<Option<DirectoryScan>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let selected = app.dialog().file().blocking_pick_folder();
        let Some(selected) = selected else {
            return Ok(None);
        };
        let path = selected
            .into_path()
            .map_err(|error| format!("无法读取所选目录路径: {error}"))?;
        allow_directory(&app, &path)?;

        scan_directory_entries(&path).map(Some)
    })
    .await
    .map_err(|error| format!("打开目录任务失败: {error}"))?
}

/// 扫描已由系统对话框授权的目录。
#[tauri::command]
pub async fn scan_directory(app: AppHandle, path: String) -> Result<Vec<ImageEntry>, String> {
    let directory = PathBuf::from(path);
    if !app.asset_protocol_scope().is_allowed(&directory) {
        return Err(format!("目录未经过用户授权: {}", directory.display()));
    }

    tauri::async_runtime::spawn_blocking(move || {
        scan_directory_entries(&directory).map(|result| result.entries)
    })
    .await
    .map_err(|error| format!("扫描目录任务失败: {error}"))?
}

/// 判断路径扩展名是否为支持的图片格式。
pub fn is_supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            IMAGE_EXTENSIONS
                .iter()
                .any(|supported| extension.eq_ignore_ascii_case(supported))
        })
}

/// 按数字感知规则比较文件名。
pub fn natural_compare(left: &str, right: &str) -> Ordering {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let (mut left_index, mut right_index) = (0, 0);

    while left_index < left.len() && right_index < right.len() {
        if left[left_index].is_ascii_digit() && right[right_index].is_ascii_digit() {
            let left_end = digit_end(left, left_index);
            let right_end = digit_end(right, right_index);
            let ordering =
                compare_digit_runs(&left[left_index..left_end], &right[right_index..right_end]);
            if ordering != Ordering::Equal {
                return ordering;
            }
            left_index = left_end;
            right_index = right_end;
            continue;
        }

        let ordering = left[left_index]
            .to_ascii_lowercase()
            .cmp(&right[right_index].to_ascii_lowercase());
        if ordering != Ordering::Equal {
            return ordering;
        }
        left_index += 1;
        right_index += 1;
    }

    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

/// 扫描目录当前层级，返回自然排序后的图片信息。
pub fn scan_directory_entries(directory: &Path) -> Result<DirectoryScan, String> {
    if !directory.is_dir() {
        return Err(format!("路径不是目录: {}", directory.display()));
    }

    let directory_entries = fs::read_dir(directory)
        .map_err(|error| format!("读取目录 {} 失败: {error}", directory.display()))?;
    let paths = directory_entries
        .filter_map(Result::ok)
        .map(|entry| entry.path());
    let entries = collect_image_entries(paths)?;

    Ok(DirectoryScan {
        directory: directory.to_string_lossy().into_owned(),
        entries,
    })
}

/// 将支持的图片路径转换为条目；单个文件读取失败时跳过。
pub fn collect_image_entries<I>(paths: I) -> Result<Vec<ImageEntry>, String>
where
    I: IntoIterator<Item = PathBuf>,
{
    let mut entries: Vec<ImageEntry> = paths
        .into_iter()
        .filter(|path| is_supported_image(path))
        .filter_map(|path| image_entry(&path).ok())
        .collect();
    entries.sort_by(|left, right| natural_compare(&left.name, &right.name));
    Ok(entries)
}

/// M2 保持静态 scope 为空，仅在运行时授权目录当前层级。
fn allow_directory(app: &AppHandle, directory: &Path) -> Result<(), String> {
    app.asset_protocol_scope()
        .allow_directory(directory, false)
        .map_err(|error| format!("授权图片目录 {} 失败: {error}", directory.display()))
}

/// 处理 argv、Apple Event 或拖放提供的外部路径。
#[tauri::command]
pub async fn open_external_path(app: AppHandle, path: String) -> Result<DirectoryScan, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = PathBuf::from(path)
            .canonicalize()
            .map_err(|error| format!("无法读取打开路径: {error}"))?;
        if path.is_dir() {
            allow_directory(&app, &path)?;
            return scan_directory_entries(&path);
        }
        if !is_supported_image(&path) {
            return Err(format!("不支持的图片格式: {}", path.display()));
        }
        let parent = path
            .parent()
            .ok_or_else(|| format!("无法确定图片父目录: {}", path.display()))?;
        allow_directory(&app, parent)?;
        let entry = image_entry(&path)?;
        Ok(DirectoryScan {
            directory: parent.to_string_lossy().into_owned(),
            entries: vec![entry],
        })
    })
    .await
    .map_err(|error| format!("打开外部路径任务失败: {error}"))?
}

/// 从启动参数中提取存在的图片或目录路径。
pub fn extract_open_paths<I, S>(args: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .skip(1)
        .map(|arg| PathBuf::from(arg.as_ref()))
        .filter(|path| path.exists() && (path.is_dir() || is_supported_image(path)))
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

fn image_entry(path: &Path) -> Result<ImageEntry, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("读取图片信息 {} 失败: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("图片路径不是文件: {}", path.display()));
    }
    let modified = metadata
        .modified()
        .map_err(|error| format!("读取图片修改时间 {} 失败: {error}", path.display()))?
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("图片修改时间早于 Unix 纪元 {}: {error}", path.display()))?
        .as_millis()
        .try_into()
        .map_err(|_| format!("图片修改时间超出支持范围: {}", path.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| format!("无法读取图片文件名: {}", path.display()))?
        .to_string_lossy()
        .into_owned();

    Ok(ImageEntry {
        path: path.to_string_lossy().into_owned(),
        name,
        size: metadata.len(),
        modified,
    })
}

fn digit_end(value: &[u8], start: usize) -> usize {
    value[start..]
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .map_or(value.len(), |offset| start + offset)
}

fn compare_digit_runs(left: &[u8], right: &[u8]) -> Ordering {
    let left_trimmed = trim_leading_zeroes(left);
    let right_trimmed = trim_leading_zeroes(right);

    left_trimmed
        .len()
        .cmp(&right_trimmed.len())
        .then_with(|| left_trimmed.cmp(right_trimmed))
        .then_with(|| left.len().cmp(&right.len()))
}

fn trim_leading_zeroes(value: &[u8]) -> &[u8] {
    let first_non_zero = value
        .iter()
        .position(|byte| *byte != b'0')
        .unwrap_or(value.len().saturating_sub(1));
    &value[first_non_zero..]
}
