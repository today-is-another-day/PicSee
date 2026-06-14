use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use tauri::{AppHandle, Manager};

/// 应用设置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    /// 界面语言。
    pub language: Language,
    /// 界面主题。
    pub theme: Theme,
    /// 用户自定义快捷键；Rust 仅负责透传持久化。
    #[serde(default)]
    pub shortcuts: BTreeMap<String, String>,
    /// 查看器设置。
    pub viewer: ViewerSettings,
    /// 大图处理设置。
    pub large_image: LargeImageSettings,
    /// 缓存设置。
    pub cache: CacheSettings,
    /// 性能设置。
    pub performance: PerformanceSettings,
    /// 布局设置。
    pub layout: LayoutSettings,
}

/// 界面语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[serde(rename = "system")]
    System,
    #[serde(rename = "zh-CN")]
    ZhCn,
    #[serde(rename = "en-US")]
    EnUs,
}

/// 界面主题。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    System,
    Light,
    Dark,
}

/// 默认缩放模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DefaultZoomMode {
    FitWindow,
    FitWidth,
    ActualSize,
    Remember,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NavigatorMode {
    Always,
    Auto,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ViewerBackground {
    Dark,
    Light,
    Checkerboard,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u32)]
pub enum NavigatorSize {
    Size160 = 160,
    Size200 = 200,
    Size240 = 240,
}

/// 缩略图栏位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThumbnailPosition {
    Left,
    Bottom,
}

/// 缩略图尺寸（最长边像素）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u32)]
pub enum ThumbnailSize {
    Size96 = 96,
    Size160 = 160,
    Size256 = 256,
}

/// 大图预览最大边长。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u32)]
pub enum PreviewMaxSize {
    Size2048 = 2048,
    Size4096 = 4096,
    Size8192 = 8192,
}

/// 瓦片边长。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u32)]
pub enum TileSize {
    Size256 = 256,
    Size512 = 512,
    Size1024 = 1024,
}

/// 查看器设置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ViewerSettings {
    /// 默认缩放模式。
    pub default_zoom_mode: DefaultZoomMode,
    /// 单次缩放步长。
    pub zoom_step: f64,
    /// 是否启用平滑缩放。
    pub smooth_zoom: bool,
    /// 是否以光标位置为缩放中心。
    pub zoom_to_cursor: bool,
    /// 切换图片时是否重置缩放。
    pub reset_zoom_on_switch: bool,
    /// 导航窗显示模式。
    pub navigator_mode: NavigatorMode,
    /// 导航窗长边尺寸。
    pub navigator_size: NavigatorSize,
    /// 删除前是否确认。
    pub confirm_delete: bool,
    /// 查看区背景。
    pub viewer_background: ViewerBackground,
    /// 自定义查看区背景色。
    pub viewer_background_color: String,
}

/// 大图处理设置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LargeImageSettings {
    /// 判定为大图的文件大小阈值，单位 MB。
    #[serde(rename = "fileSizeThresholdMB")]
    pub file_size_threshold_mb: u64,
    /// 判定为大图的像素总量阈值。
    pub pixel_threshold: u64,
    /// 判定为大图的单边长度阈值。
    pub side_threshold: u32,
    /// 大图预览最大边长。
    pub preview_max_size: PreviewMaxSize,
    /// 瓦片边长。
    pub tile_size: TileSize,
    /// 是否启用瓦片预取。
    pub enable_tile_prefetch: bool,
    /// 瓦片预取半径。
    pub prefetch_radius: u32,
}

/// 缓存设置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CacheSettings {
    /// 内存缓存上限，单位 MB。
    #[serde(rename = "memoryCacheLimitMB")]
    pub memory_cache_limit_mb: u64,
    /// 磁盘缓存上限，单位 MB。
    #[serde(rename = "diskCacheLimitMB")]
    pub disk_cache_limit_mb: u64,
    /// 是否启用磁盘缓存。
    pub enable_disk_cache: bool,
    /// 退出时是否清理临时瓦片。
    pub clear_temp_tile_on_exit: bool,
}

/// 性能设置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PerformanceSettings {
    /// 瓦片处理并发数。
    pub tile_concurrency: u32,
    /// 图片解码并发数。
    pub decode_concurrency: u32,
    /// 缩略图生成并发数。
    pub thumbnail_concurrency: u32,
    /// CPU 解码线程数（大图预览/瓦片/缩略图并行解码用）。
    pub cpu_threads: u32,
    /// 普通图片预加载数量。
    pub preload_normal_count: u32,
    /// 大图预览预加载数量。
    pub preload_large_preview_count: u32,
}

/// 布局设置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LayoutSettings {
    /// 是否显示缩略图栏。
    pub show_thumbnail_bar: bool,
    /// 缩略图栏位置。
    pub thumbnail_position: ThumbnailPosition,
    /// 缩略图尺寸（最长边像素）。
    pub thumbnail_size: ThumbnailSize,
    /// 是否显示状态栏。
    pub show_status_bar: bool,
    /// 是否启用紧凑模式。
    pub compact_mode: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            language: Language::System,
            theme: Theme::System,
            shortcuts: BTreeMap::new(),
            viewer: ViewerSettings::default(),
            large_image: LargeImageSettings::default(),
            cache: CacheSettings::default(),
            performance: PerformanceSettings::default(),
            layout: LayoutSettings::default(),
        }
    }
}

impl Default for ViewerSettings {
    fn default() -> Self {
        Self {
            default_zoom_mode: DefaultZoomMode::FitWindow,
            zoom_step: 0.1,
            smooth_zoom: true,
            zoom_to_cursor: true,
            reset_zoom_on_switch: true,
            navigator_mode: NavigatorMode::Auto,
            navigator_size: NavigatorSize::Size200,
            confirm_delete: false,
            viewer_background: ViewerBackground::Dark,
            viewer_background_color: "#202020".to_string(),
        }
    }
}

impl Default for LargeImageSettings {
    fn default() -> Self {
        Self {
            file_size_threshold_mb: 300,
            pixel_threshold: 50_000_000,
            side_threshold: 12_000,
            preview_max_size: PreviewMaxSize::Size4096,
            tile_size: TileSize::Size512,
            enable_tile_prefetch: true,
            prefetch_radius: 1,
        }
    }
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            memory_cache_limit_mb: 512,
            disk_cache_limit_mb: 2048,
            enable_disk_cache: true,
            clear_temp_tile_on_exit: true,
        }
    }
}

impl Default for PerformanceSettings {
    fn default() -> Self {
        Self {
            tile_concurrency: 4,
            decode_concurrency: 2,
            thumbnail_concurrency: 4,
            cpu_threads: 8,
            preload_normal_count: 2,
            preload_large_preview_count: 1,
        }
    }
}

impl Default for LayoutSettings {
    fn default() -> Self {
        Self {
            show_thumbnail_bar: true,
            thumbnail_position: ThumbnailPosition::Bottom,
            thumbnail_size: ThumbnailSize::Size160,
            show_status_bar: true,
            compact_mode: false,
        }
    }
}

/// 从指定路径读取设置；文件不存在时返回默认设置。
pub fn read_settings_file(path: &Path) -> Result<AppSettings, String> {
    if !path.exists() {
        return Ok(AppSettings::default());
    }

    let content = fs::read_to_string(path)
        .map_err(|error| format!("读取设置文件 {} 失败: {error}", path.display()))?;
    match serde_json::from_str(&content) {
        Ok(settings) => Ok(settings),
        Err(_) => {
            let backup_path = backup_path(path);
            if backup_path.exists() {
                fs::remove_file(&backup_path).map_err(|error| {
                    format!("删除旧设置备份 {} 失败: {error}", backup_path.display())
                })?;
            }
            fs::rename(path, &backup_path).map_err(|error| {
                format!(
                    "备份损坏设置文件 {} 到 {} 失败: {error}",
                    path.display(),
                    backup_path.display()
                )
            })?;
            Ok(AppSettings::default())
        }
    }
}

/// 将设置写入同目录临时文件后原子替换目标文件。
pub fn write_settings_file(path: &Path, settings: &AppSettings) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("创建设置目录 {} 失败: {error}", parent.display()))?;
    }

    let content = serde_json::to_string_pretty(settings)
        .map_err(|error| format!("序列化设置失败: {error}"))?;
    let temporary_path = temporary_path(path);
    fs::write(&temporary_path, content).map_err(|error| {
        format!(
            "写入临时设置文件 {} 失败: {error}",
            temporary_path.display()
        )
    })?;
    fs::rename(&temporary_path, path).map_err(|error| {
        let _ = fs::remove_file(&temporary_path);
        format!(
            "用临时设置文件 {} 替换 {} 失败: {error}",
            temporary_path.display(),
            path.display()
        )
    })
}

fn backup_path(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        "{}.bak",
        path.file_name().unwrap_or_default().to_string_lossy()
    ))
}

fn temporary_path(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        "{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ))
}

/// 获取应用配置目录中的设置文件路径。
fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|directory| directory.join("settings.json"))
        .map_err(|error| format!("获取应用配置目录失败: {error}"))
}

/// 读取 PicSee 设置。
#[tauri::command]
pub fn get_settings(app: AppHandle) -> Result<AppSettings, String> {
    read_settings_file(&settings_path(&app)?)
}

/// 保存 PicSee 设置。
#[tauri::command]
pub fn save_settings(app: AppHandle, settings: AppSettings) -> Result<(), String> {
    write_settings_file(&settings_path(&app)?, &settings)
}
