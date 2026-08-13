//! 系统格式（HEIC/HEIF/TIFF/RAW）解码后端：把“调用外部解码器”的平台差异收敛于此。
//!
//! - macOS：`sips`（ImageIO/ColorSync），保持原有行为完全不变。
//! - 其他平台（Linux 等）：libvips（`vipsheader` 读尺寸、`vipsthumbnail` 解码并转 sRGB）。
//!
//! 上层 [`crate::extended_formats`] 负责缓存、尺寸安全校验、子进程超时与临时文件管理，
//! 这些逻辑与平台无关；本模块只构造命令、解析 probe 输出，并提供后端名称用于错误信息。
//!
//! 每个函数都返回构造好的 [`std::process::Command`]，由上层统一执行（带超时、捕获 stdout/stderr），
//! 保证 macOS 与 Linux 共享同一套执行/超时/缓存代码路径。

use std::path::Path;
use std::process::Command;

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    /// 后端名称，用于错误信息。
    pub const NAME: &str = "macOS ImageIO/ColorSync (sips)";

    /// 仅读尺寸元数据，不解码、不生成临时文件。
    pub fn probe_command(path: &Path) -> Result<Command, String> {
        let mut command = Command::new("sips");
        command
            .args(["-g", "pixelWidth", "-g", "pixelHeight"])
            .arg(path);
        Ok(command)
    }

    /// 解析 `sips -g pixelWidth -g pixelHeight` 的输出。
    pub fn parse_probe_output(stdout: &str) -> Result<(u32, u32), String> {
        let width = parse_sips_property(stdout, "pixelWidth")?;
        let height = parse_sips_property(stdout, "pixelHeight")?;
        Ok((width, height))
    }

    fn parse_sips_property(output: &str, property: &str) -> Result<u32, String> {
        output
            .lines()
            .find_map(|line| {
                let (key, value) = line.trim().split_once(':')?;
                (key == property).then(|| value.trim().parse::<u32>().ok())?
            })
            .ok_or_else(|| format!("sips 输出缺少 {property}"))
    }

    /// 全量解码为 sRGB PNG（ColorSync 强制转换到 sRGB）。
    pub fn decode_command(path: &Path, output: &Path) -> Result<Command, String> {
        let mut command = Command::new("sips");
        command
            .args(["-s", "format", "png"])
            .args(["-m", "/System/Library/ColorSync/Profiles/sRGB Profile.icc"])
            .arg(path)
            .arg("--out")
            .arg(output);
        Ok(command)
    }

    /// 降采样为最长边 ≤ `max_side` 的 PNG（解码在子进程内完成，内存隔离）。
    pub fn downscale_command(path: &Path, max_side: u32, output: &Path) -> Result<Command, String> {
        let mut command = Command::new("sips");
        command
            .args(["-s", "format", "png"])
            .args(["-Z", &max_side.to_string()])
            .arg(path)
            .arg("--out")
            .arg(output);
        Ok(command)
    }

    /// 直接解码为未压缩 BMP 栅格（可选降采样），供大图引擎按需分块读取。
    ///
    /// 相比 PNG 中转，省掉了 PNG 压缩/解压与“整图读入本进程内存”两步：
    /// 实测 1.5 亿像素 AVIF 输出 PNG 约 6.3s，输出 BMP 约 1.0s，
    /// 且 BMP 可被 [`crate::large_image::bmp::BmpReader`] 随机读取，本进程零整图驻留。
    pub fn raster_bmp_command(
        path: &Path,
        max_side: Option<u32>,
        output: &Path,
    ) -> Option<Command> {
        let mut command = Command::new("sips");
        command.args(["-s", "format", "bmp"]);
        if let Some(max_side) = max_side {
            command.args(["-Z", &max_side.to_string()]);
        }
        command
            .args(["-m", "/System/Library/ColorSync/Profiles/sRGB Profile.icc"])
            .arg(path)
            .arg("--out")
            .arg(output);
        Some(command)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    /// 后端名称，用于错误信息。
    pub const NAME: &str = "libvips";

    /// vipsthumbnail 默认会把小图放大到 size 框，因此用 `>` 修饰符表示“仅缩小、不放大”，
    /// 使全尺寸解码（超大边界）对任意图都返回原尺寸，且与 macOS `sips -Z`（no larger）语义一致。
    const FULL_DECODE_MAX_SIDE: u32 = 65_535;

    /// 仅读尺寸元数据（`vipsheader` 不解码像素）。
    pub fn probe_command(path: &Path) -> Result<Command, String> {
        let mut command = Command::new("vipsheader");
        command.arg(path);
        Ok(command)
    }

    /// 解析 `vipsheader` 输出。首行形如：
    /// `/path/img.heic: 4032x3024 uchar, 3 bands, srgb, heifload`
    ///
    /// 用扫描法取第一个 `数字x数字` token，避免依赖文件名/字段顺序；
    /// 路径中形如 `2x3` 的目录名不会被误判（其右侧含非数字字符，parse 失败）。
    pub fn parse_probe_output(stdout: &str) -> Result<(u32, u32), String> {
        for token in stdout.split(|c: char| c.is_whitespace() || c == ',') {
            if let Some((w, h)) = token.split_once('x') {
                if let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) {
                    return Ok((w, h));
                }
            }
        }
        Err("vipsheader 输出缺少尺寸信息".to_string())
    }

    /// 全量解码为 sRGB PNG。`--export-profile srgb` 做色彩管理：
    /// 有嵌入 ICC 时从嵌入转 sRGB，无嵌入时按默认（sRGB）假设输出。
    pub fn decode_command(path: &Path, output: &Path) -> Result<Command, String> {
        Ok(vipsthumbnail(path, FULL_DECODE_MAX_SIDE, output))
    }

    /// 降采样为最长边 ≤ `max_side` 的 sRGB PNG（解码在子进程内完成，内存隔离）。
    pub fn downscale_command(path: &Path, max_side: u32, output: &Path) -> Result<Command, String> {
        Ok(vipsthumbnail(path, max_side, output))
    }

    fn vipsthumbnail(path: &Path, max_side: u32, output: &Path) -> Command {
        let mut command = Command::new("vipsthumbnail");
        command
            .arg(path)
            // `>` = 仅缩小、不放大（不经 shell，作字面参数传给 vipsthumbnail）。
            .arg("--size")
            .arg(format!("{max_side}x{max_side}>"))
            .arg("--export-profile")
            .arg("srgb")
            .arg("-o")
            .arg(output);
        command
    }

    /// libvips 不写 BMP，本平台不提供 BMP 栅格直转；调用方回退到 PNG 预览路径。
    pub fn raster_bmp_command(
        _path: &Path,
        _max_side: Option<u32>,
        _output: &Path,
    ) -> Option<Command> {
        None
    }
}

pub use imp::{
    decode_command, downscale_command, parse_probe_output, probe_command, raster_bmp_command, NAME,
};
