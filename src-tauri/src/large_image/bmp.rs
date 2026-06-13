use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
};

use super::LargeImageError;

/// BMP 像素格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Bgr24,
    Bgra32,
}

impl PixelFormat {
    /// 每像素字节数。
    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            PixelFormat::Bgr24 => 3,
            PixelFormat::Bgra32 => 4,
        }
    }
}

/// 图像区域（像素坐标，左上角原点）。
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// BMP 文件信息（解析自文件头）。
#[derive(Debug, Clone)]
pub struct BmpInfo {
    pub width: u32,
    pub height: u32,
    pub pixel_format: PixelFormat,
    /// 行方向：true = 底部在文件前（标准 BMP）；false = 顶部在文件前。
    pub bottom_up: bool,
    /// 像素数据在文件中的起始偏移（字节）。
    pub pixel_data_offset: u64,
    /// 每行字节数（含 4 字节对齐填充）。
    pub row_stride: u64,
}

impl BmpInfo {
    /// 从文件路径解析 BMP 头。
    pub fn from_file(path: &Path) -> Result<Self, LargeImageError> {
        let file =
            File::open(path).map_err(|e| LargeImageError::io(format!("无法打开 BMP 文件: {e}")))?;
        Self::from_reader(&mut BufReader::new(file))
    }

    /// 从 reader 解析 BMP 头（至少读取 54 字节）。
    pub fn from_reader<R: Read + Seek>(reader: &mut R) -> Result<Self, LargeImageError> {
        let mut header = [0u8; 54];
        reader
            .read_exact(&mut header)
            .map_err(|e| LargeImageError::decode(format!("读取 BMP 头失败: {e}")))?;

        // 魔数 'BM'
        if &header[0..2] != b"BM" {
            return Err(LargeImageError::decode("不是有效的 BMP 文件（魔数错误）"));
        }

        // 像素数据偏移（offset 10）
        let pixel_data_offset = u32::from_le_bytes(header[10..14].try_into().unwrap()) as u64;

        // DIB header size（offset 14），必须 >= 40
        let dib_size = u32::from_le_bytes(header[14..18].try_into().unwrap());
        if dib_size < 40 {
            return Err(LargeImageError::decode(format!(
                "DIB header size {dib_size} < 40，不支持此 BMP 变体"
            )));
        }

        // width（offset 18），height（offset 22）
        let width_raw = i32::from_le_bytes(header[18..22].try_into().unwrap());
        let height_raw = i32::from_le_bytes(header[22..26].try_into().unwrap());

        let width = width_raw.unsigned_abs();
        let bottom_up = height_raw >= 0;
        let height = height_raw.unsigned_abs();

        // bit count（offset 28）
        let bit_count = u16::from_le_bytes(header[28..30].try_into().unwrap());
        // compression（offset 30）
        let compression = u32::from_le_bytes(header[30..34].try_into().unwrap());

        if compression != 0 {
            return Err(LargeImageError::unsupported_format(format!(
                "BMP compression={compression}，仅支持 BI_RGB (0)"
            )));
        }

        let pixel_format = match bit_count {
            24 => PixelFormat::Bgr24,
            32 => PixelFormat::Bgra32,
            _ => {
                return Err(LargeImageError::unsupported_format(format!(
                    "BMP bit_count={bit_count}，仅支持 24 和 32"
                )))
            }
        };

        let bpp = pixel_format.bytes_per_pixel() as u64;
        // 每行字节数（含 4 字节对齐填充）
        let row_stride = (width as u64 * bpp + 3) & !3;

        Ok(BmpInfo {
            width,
            height,
            pixel_format,
            bottom_up,
            pixel_data_offset,
            row_stride,
        })
    }

    /// 计算第 row 行（图像坐标，0=顶部）在文件中的字节偏移。
    pub fn row_file_offset(&self, row: u32) -> u64 {
        let file_row = if self.bottom_up {
            self.height - 1 - row
        } else {
            row
        };
        self.pixel_data_offset + file_row as u64 * self.row_stride
    }
}

/// BMP 读取器（每次操作重新 seek）。
pub struct BmpReader {
    pub info: BmpInfo,
    path: std::path::PathBuf,
}

impl BmpReader {
    /// 打开 BMP 文件并解析头。
    pub fn open(path: &Path) -> Result<Self, LargeImageError> {
        let info = BmpInfo::from_file(path)?;
        Ok(BmpReader {
            info,
            path: path.to_path_buf(),
        })
    }

    /// 读取指定区域并缩放到 target_width×target_height，返回 RGBA 字节。
    ///
    /// - 区域超出图像边界时自动 clamp。
    /// - 区域完全在图像外时返回全零（透明黑）。
    pub fn read_region(
        &self,
        rect: Rect,
        target_width: u32,
        target_height: u32,
    ) -> Result<Vec<u8>, LargeImageError> {
        let img_w = self.info.width;
        let img_h = self.info.height;

        // clamp rect 到图像边界
        let src_x = rect.x.min(img_w);
        let src_y = rect.y.min(img_h);
        let src_x2 = (rect.x.saturating_add(rect.width)).min(img_w);
        let src_y2 = (rect.y.saturating_add(rect.height)).min(img_h);
        let src_w = src_x2.saturating_sub(src_x);
        let src_h = src_y2.saturating_sub(src_y);

        // 区域完全超出图像
        if src_w == 0 || src_h == 0 || target_width == 0 || target_height == 0 {
            return Ok(vec![
                0u8;
                target_width as usize * target_height as usize * 4
            ]);
        }

        let bpp = self.info.pixel_format.bytes_per_pixel() as usize;

        // 重新打开文件（BufReader<File> 不是 Send，不缓存）
        let file = File::open(&self.path)
            .map_err(|e| LargeImageError::io(format!("打开 BMP 文件失败: {e}")))?;
        let mut reader = BufReader::new(file);

        let mut output = vec![0u8; target_width as usize * target_height as usize * 4];

        for ty in 0..target_height {
            // nearest neighbor：目标行 ty → 源行 src_row
            let src_row = src_y + (ty * src_h) / target_height;
            let row_offset = self.info.row_file_offset(src_row);
            let col_offset = src_x as u64 * bpp as u64;
            let row_bytes_needed = src_w as usize * bpp;

            reader
                .seek(SeekFrom::Start(row_offset + col_offset))
                .map_err(|e| LargeImageError::io(format!("seek 失败: {e}")))?;

            let mut row_buf = vec![0u8; row_bytes_needed];
            reader
                .read_exact(&mut row_buf)
                .map_err(|e| LargeImageError::decode(format!("读取行数据失败: {e}")))?;

            for tx in 0..target_width {
                // nearest neighbor：目标列 tx → 源列 src_col
                let src_col = (tx * src_w) / target_width;
                let src_off = src_col as usize * bpp;

                let out_off = (ty as usize * target_width as usize + tx as usize) * 4;

                match self.info.pixel_format {
                    PixelFormat::Bgr24 => {
                        let b = row_buf[src_off];
                        let g = row_buf[src_off + 1];
                        let r = row_buf[src_off + 2];
                        output[out_off] = r;
                        output[out_off + 1] = g;
                        output[out_off + 2] = b;
                        output[out_off + 3] = 255;
                    }
                    PixelFormat::Bgra32 => {
                        let b = row_buf[src_off];
                        let g = row_buf[src_off + 1];
                        let r = row_buf[src_off + 2];
                        let a = row_buf[src_off + 3];
                        output[out_off] = r;
                        output[out_off + 1] = g;
                        output[out_off + 2] = b;
                        output[out_off + 3] = a;
                    }
                }
            }
        }

        Ok(output)
    }
}

// ─────────────────────────── 测试 ───────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// 生成测试用 BMP 文件字节（含完整像素数据）。
    ///
    /// 像素填充规则（图像坐标，0=顶部）：
    /// - B = 0
    /// - G = img_y % 256
    /// - R = img_x % 256
    /// - A = 255（32bit）
    fn make_bmp_raw(width: u32, height: u32, bottom_up: bool, is_32bit: bool) -> Vec<u8> {
        let bpp: u32 = if is_32bit { 4 } else { 3 };
        let row_stride = (width * bpp + 3) & !3;
        let pixel_data_size = row_stride * height;
        let file_size = 54 + pixel_data_size;

        let mut data = vec![0u8; file_size as usize];

        // BM 魔数
        data[0] = b'B';
        data[1] = b'M';
        // 文件大小
        data[2..6].copy_from_slice(&file_size.to_le_bytes());
        // 保留
        data[6..10].copy_from_slice(&0u32.to_le_bytes());
        // 像素数据偏移
        data[10..14].copy_from_slice(&54u32.to_le_bytes());
        // DIB header size = 40
        data[14..18].copy_from_slice(&40u32.to_le_bytes());
        // width
        data[18..22].copy_from_slice(&(width as i32).to_le_bytes());
        // height（负 = top-down）
        let height_raw: i32 = if bottom_up {
            height as i32
        } else {
            -(height as i32)
        };
        data[22..26].copy_from_slice(&height_raw.to_le_bytes());
        // planes = 1
        data[26..28].copy_from_slice(&1u16.to_le_bytes());
        // bit count
        let bit_count: u16 = if is_32bit { 32 } else { 24 };
        data[28..30].copy_from_slice(&bit_count.to_le_bytes());
        // compression = 0 (BI_RGB)
        data[30..34].copy_from_slice(&0u32.to_le_bytes());

        // 写入像素数据
        for img_y in 0..height {
            let file_row = if bottom_up { height - 1 - img_y } else { img_y };
            let row_start = 54 + file_row as usize * row_stride as usize;
            for img_x in 0..width {
                let off = row_start + img_x as usize * bpp as usize;
                data[off] = 0; // B
                data[off + 1] = (img_y % 256) as u8; // G
                data[off + 2] = (img_x % 256) as u8; // R
                if is_32bit {
                    data[off + 3] = 255; // A
                }
            }
        }

        data
    }

    fn write_temp_bmp(width: u32, height: u32, bottom_up: bool, is_32bit: bool) -> NamedTempFile {
        let raw = make_bmp_raw(width, height, bottom_up, is_32bit);
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(&raw).unwrap();
        f.flush().unwrap();
        f
    }

    // ── BmpInfo 解析 ──

    #[test]
    fn test_bmp_info_24bit_bottom_up() {
        let f = write_temp_bmp(10, 8, true, false);
        let info = BmpInfo::from_file(f.path()).unwrap();
        assert_eq!(info.width, 10);
        assert_eq!(info.height, 8);
        assert_eq!(info.pixel_format, PixelFormat::Bgr24);
        assert!(info.bottom_up);
        // row_stride = (10*3 + 3) & !3 = 33 & !3 = 32
        assert_eq!(info.row_stride, 32);
    }

    #[test]
    fn test_bmp_info_top_down() {
        let f = write_temp_bmp(10, 8, false, false);
        let info = BmpInfo::from_file(f.path()).unwrap();
        assert!(!info.bottom_up);
        assert_eq!(info.width, 10);
        assert_eq!(info.height, 8);
    }

    #[test]
    fn test_bmp_info_32bit() {
        let f = write_temp_bmp(10, 8, true, true);
        let info = BmpInfo::from_file(f.path()).unwrap();
        assert_eq!(info.pixel_format, PixelFormat::Bgra32);
        // row_stride = (10*4 + 3) & !3 = 40
        assert_eq!(info.row_stride, 40);
    }

    #[test]
    fn test_bmp_info_non_aligned_width_3x2() {
        let f = write_temp_bmp(3, 2, true, false);
        let info = BmpInfo::from_file(f.path()).unwrap();
        // row_stride = (3*3 + 3) & !3 = 12
        assert_eq!(info.row_stride, 12);
    }

    #[test]
    fn test_bmp_info_non_aligned_width_5x3() {
        let f = write_temp_bmp(5, 3, true, false);
        let info = BmpInfo::from_file(f.path()).unwrap();
        // row_stride = (5*3 + 3) & !3 = 16
        assert_eq!(info.row_stride, 16);
    }

    // ── 读取像素（全区域）──

    fn full_rect(info: &BmpInfo) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: info.width,
            height: info.height,
        }
    }

    /// 验证像素 (x, y)（图像坐标）的 RGBA 值。
    fn pixel_at(rgba: &[u8], x: u32, y: u32, width: u32) -> (u8, u8, u8, u8) {
        let off = (y as usize * width as usize + x as usize) * 4;
        (rgba[off], rgba[off + 1], rgba[off + 2], rgba[off + 3])
    }

    #[test]
    fn test_read_full_region_24bit_bottom_up() {
        // 4×3 bottom-up 24bit
        let f = write_temp_bmp(4, 3, true, false);
        let reader = BmpReader::open(f.path()).unwrap();
        let rect = full_rect(&reader.info);
        let rgba = reader.read_region(rect, 4, 3).unwrap();
        // 像素 (x=2, y=1)：R=2, G=1, B=0, A=255
        let (r, g, b, a) = pixel_at(&rgba, 2, 1, 4);
        assert_eq!((r, g, b, a), (2, 1, 0, 255));
    }

    #[test]
    fn test_read_full_region_24bit_top_down() {
        let f = write_temp_bmp(4, 3, false, false);
        let reader = BmpReader::open(f.path()).unwrap();
        let rect = full_rect(&reader.info);
        let rgba = reader.read_region(rect, 4, 3).unwrap();
        let (r, g, b, a) = pixel_at(&rgba, 3, 2, 4);
        assert_eq!((r, g, b, a), (3, 2, 0, 255));
    }

    #[test]
    fn test_read_full_region_32bit() {
        let f = write_temp_bmp(4, 3, true, true);
        let reader = BmpReader::open(f.path()).unwrap();
        let rect = full_rect(&reader.info);
        let rgba = reader.read_region(rect, 4, 3).unwrap();
        let (r, g, b, a) = pixel_at(&rgba, 1, 2, 4);
        assert_eq!((r, g, b, a), (1, 2, 0, 255));
    }

    #[test]
    fn test_read_non_aligned_width_3x2() {
        let f = write_temp_bmp(3, 2, true, false);
        let reader = BmpReader::open(f.path()).unwrap();
        let rect = full_rect(&reader.info);
        let rgba = reader.read_region(rect, 3, 2).unwrap();
        // 像素 (2, 1): R=2, G=1, B=0, A=255
        let (r, g, b, a) = pixel_at(&rgba, 2, 1, 3);
        assert_eq!((r, g, b, a), (2, 1, 0, 255));
    }

    #[test]
    fn test_read_non_aligned_width_5x3() {
        let f = write_temp_bmp(5, 3, true, false);
        let reader = BmpReader::open(f.path()).unwrap();
        let rect = full_rect(&reader.info);
        let rgba = reader.read_region(rect, 5, 3).unwrap();
        // 像素 (4, 2): R=4, G=2, B=0, A=255
        let (r, g, b, a) = pixel_at(&rgba, 4, 2, 5);
        assert_eq!((r, g, b, a), (4, 2, 0, 255));
    }

    #[test]
    fn test_read_sub_region() {
        // 读取 (1,1) → 2×2 子区域
        let f = write_temp_bmp(8, 8, true, false);
        let reader = BmpReader::open(f.path()).unwrap();
        let rect = Rect {
            x: 1,
            y: 1,
            width: 2,
            height: 2,
        };
        let rgba = reader.read_region(rect, 2, 2).unwrap();
        // 子区域 (0,0) = 图像 (1,1): R=1, G=1, B=0, A=255
        let (r, g, b, a) = pixel_at(&rgba, 0, 0, 2);
        assert_eq!((r, g, b, a), (1, 1, 0, 255));
        // 子区域 (1,1) = 图像 (2,2): R=2, G=2, B=0, A=255
        let (r, g, b, a) = pixel_at(&rgba, 1, 1, 2);
        assert_eq!((r, g, b, a), (2, 2, 0, 255));
    }

    #[test]
    fn test_read_region_boundary_clamp() {
        // rect 超出边界，应被 clamp
        let f = write_temp_bmp(4, 4, true, false);
        let reader = BmpReader::open(f.path()).unwrap();
        // 从 (2,2) 请求 4×4，实际只有 2×2 图像范围
        let rect = Rect {
            x: 2,
            y: 2,
            width: 4,
            height: 4,
        };
        // 正常返回，不 panic
        let rgba = reader.read_region(rect, 2, 2).unwrap();
        assert_eq!(rgba.len(), 2 * 2 * 4);
    }

    #[test]
    fn test_read_region_fully_out_of_bounds() {
        let f = write_temp_bmp(4, 4, true, false);
        let reader = BmpReader::open(f.path()).unwrap();
        // 从 (10,10) 请求区域，完全超出
        let rect = Rect {
            x: 10,
            y: 10,
            width: 2,
            height: 2,
        };
        let rgba = reader.read_region(rect, 2, 2).unwrap();
        // 应返回全零（透明黑）
        assert!(rgba.iter().all(|&b| b == 0));
    }

    #[test]
    fn test_downsampled_read() {
        // 8×8 → 2×2 降采样
        let f = write_temp_bmp(8, 8, true, false);
        let reader = BmpReader::open(f.path()).unwrap();
        let rect = full_rect(&reader.info);
        let rgba = reader.read_region(rect, 2, 2).unwrap();
        assert_eq!(rgba.len(), 2 * 2 * 4);
        // 结果长度正确即可（nearest neighbor 无数学断言）
    }

    #[test]
    fn test_downsampled_known_pattern() {
        // 4×1 → 2×1，验证 nearest 列映射
        // 像素 R：x=0→0, x=1→1, x=2→2, x=3→3
        let f = write_temp_bmp(4, 1, true, false);
        let reader = BmpReader::open(f.path()).unwrap();
        let rect = Rect {
            x: 0,
            y: 0,
            width: 4,
            height: 1,
        };
        let rgba = reader.read_region(rect, 2, 1).unwrap();
        // tx=0: src_col = (0 * 4) / 2 = 0 → R=0
        // tx=1: src_col = (1 * 4) / 2 = 2 → R=2
        let (r0, _, _, _) = pixel_at(&rgba, 0, 0, 2);
        let (r1, _, _, _) = pixel_at(&rgba, 1, 0, 2);
        assert_eq!(r0, 0);
        assert_eq!(r1, 2);
    }

    // ── 错误情况 ──

    #[test]
    fn test_unsupported_compression() {
        let mut raw = make_bmp_raw(4, 4, true, false);
        // compression = 1（RLE8）
        raw[30..34].copy_from_slice(&1u32.to_le_bytes());
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(&raw).unwrap();
        f.flush().unwrap();

        let result = BmpInfo::from_file(f.path());
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, "UNSUPPORTED_FORMAT");
    }

    #[test]
    fn test_unsupported_16bit() {
        let mut raw = make_bmp_raw(4, 4, true, false);
        // bit_count = 16
        raw[28..30].copy_from_slice(&16u16.to_le_bytes());
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(&raw).unwrap();
        f.flush().unwrap();

        let result = BmpInfo::from_file(f.path());
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, "UNSUPPORTED_FORMAT");
    }
}
