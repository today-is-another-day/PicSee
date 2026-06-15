use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};

use super::{
    bmp::{BmpReader, PixelFormat, Rect},
    LargeImageError,
};

/// 把 24/32-bit BI_RGB BMP 以 2×2 盒式平均降采样为 top-down BI_RGB BMP。
///
/// 调用方负责使用 `.part` 目标路径并在成功后原子 rename。
pub fn generate_downscaled_raster(src: &Path, dst: &Path) -> Result<(u32, u32), LargeImageError> {
    let reader = BmpReader::open(src)?;
    let (src_width, src_height) = (reader.info.width, reader.info.height);
    let (dst_width, dst_height) = (src_width.div_ceil(2), src_height.div_ceil(2));
    let has_alpha = matches!(reader.info.pixel_format, PixelFormat::Bgra32);
    let channels = if has_alpha { 4usize } else { 3usize };
    let dst_row_stride = (dst_width as usize * channels + 3) & !3;
    let file_size = 54u64 + dst_row_stride as u64 * dst_height as u64;

    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| LargeImageError::io(format!("创建金字塔目录失败: {e}")))?;
    }
    let file =
        File::create(dst).map_err(|e| LargeImageError::io(format!("创建金字塔栅格失败: {e}")))?;
    let mut writer = BufWriter::new(file);

    let mut header = [0u8; 54];
    header[0..2].copy_from_slice(b"BM");
    header[2..6].copy_from_slice(&(file_size.min(u32::MAX as u64) as u32).to_le_bytes());
    header[10..14].copy_from_slice(&54u32.to_le_bytes());
    header[14..18].copy_from_slice(&40u32.to_le_bytes());
    header[18..22].copy_from_slice(&(dst_width as i32).to_le_bytes());
    header[22..26].copy_from_slice(&(-(dst_height as i32)).to_le_bytes());
    header[26..28].copy_from_slice(&1u16.to_le_bytes());
    header[28..30].copy_from_slice(&((channels as u16) * 8).to_le_bytes());
    writer
        .write_all(&header)
        .map_err(|e| LargeImageError::io(format!("写金字塔栅格头失败: {e}")))?;

    let mut dst_row = vec![0u8; dst_row_stride];
    for dst_y in 0..dst_height {
        let src_y = dst_y * 2;
        let src_rows = 2.min(src_height - src_y);
        let rgba = reader.read_region(
            Rect {
                x: 0,
                y: src_y,
                width: src_width,
                height: src_rows,
            },
            src_width,
            src_rows,
        )?;
        dst_row.fill(0);

        for dst_x in 0..dst_width {
            let src_x = dst_x * 2;
            let src_cols = 2.min(src_width - src_x);
            let count = src_cols * src_rows;
            let mut sums = [0u32; 4];
            for row in 0..src_rows {
                for col in 0..src_cols {
                    let offset = ((row * src_width + src_x + col) * 4) as usize;
                    let [r, g, b, a] = rgba[offset..offset + 4].try_into().unwrap();
                    if has_alpha {
                        sums[0] += r as u32 * a as u32 / 255;
                        sums[1] += g as u32 * a as u32 / 255;
                        sums[2] += b as u32 * a as u32 / 255;
                        sums[3] += a as u32;
                    } else {
                        sums[0] += r as u32;
                        sums[1] += g as u32;
                        sums[2] += b as u32;
                    }
                }
            }

            let rounded_mean = |sum: u32| (sum + count / 2) / count;
            let (r, g, b, a) = if has_alpha {
                let alpha = rounded_mean(sums[3]);
                let restore = |sum: u32| {
                    if alpha == 0 {
                        0
                    } else {
                        (rounded_mean(sum) * 255 / alpha).min(255) as u8
                    }
                };
                (
                    restore(sums[0]),
                    restore(sums[1]),
                    restore(sums[2]),
                    alpha as u8,
                )
            } else {
                (
                    rounded_mean(sums[0]) as u8,
                    rounded_mean(sums[1]) as u8,
                    rounded_mean(sums[2]) as u8,
                    255,
                )
            };

            let offset = dst_x as usize * channels;
            dst_row[offset] = b;
            dst_row[offset + 1] = g;
            dst_row[offset + 2] = r;
            if has_alpha {
                dst_row[offset + 3] = a;
            }
        }
        writer
            .write_all(&dst_row)
            .map_err(|e| LargeImageError::io(format!("写金字塔栅格行失败: {e}")))?;
    }
    writer
        .flush()
        .map_err(|e| LargeImageError::io(format!("flush 金字塔栅格失败: {e}")))?;
    Ok((dst_width, dst_height))
}

#[cfg(test)]
mod tests {
    use super::generate_downscaled_raster;
    use crate::large_image::bmp::{BmpReader, Rect};
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn make_bmp<F>(width: u32, height: u32, channels: usize, mut pixel: F) -> NamedTempFile
    where
        F: FnMut(u32, u32) -> [u8; 4],
    {
        let mut file = NamedTempFile::new_in(".").unwrap();
        let row_stride = (width as usize * channels + 3) & !3;
        let file_size = 54 + row_stride * height as usize;
        let mut header = [0u8; 54];
        header[0..2].copy_from_slice(b"BM");
        header[2..6].copy_from_slice(&(file_size as u32).to_le_bytes());
        header[10..14].copy_from_slice(&54u32.to_le_bytes());
        header[14..18].copy_from_slice(&40u32.to_le_bytes());
        header[18..22].copy_from_slice(&(width as i32).to_le_bytes());
        header[22..26].copy_from_slice(&(-(height as i32)).to_le_bytes());
        header[26..28].copy_from_slice(&1u16.to_le_bytes());
        header[28..30].copy_from_slice(&((channels * 8) as u16).to_le_bytes());
        file.write_all(&header).unwrap();

        let mut row = vec![0u8; row_stride];
        for y in 0..height {
            row.fill(0);
            for x in 0..width {
                let [r, g, b, a] = pixel(x, y);
                let offset = x as usize * channels;
                row[offset] = b;
                row[offset + 1] = g;
                row[offset + 2] = r;
                if channels == 4 {
                    row[offset + 3] = a;
                }
            }
            file.write_all(&row).unwrap();
        }
        file.flush().unwrap();
        file
    }

    fn read_all(path: &std::path::Path) -> (u32, u32, Vec<u8>) {
        let reader = BmpReader::open(path).unwrap();
        let (width, height) = (reader.info.width, reader.info.height);
        let rgba = reader
            .read_region(
                Rect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                },
                width,
                height,
            )
            .unwrap();
        (width, height, rgba)
    }

    #[test]
    fn test_box_2x2_24bit_mean() {
        let src = make_bmp(4, 4, 3, |x, y| [(x * 10) as u8, (y * 10) as u8, 0, 255]);
        let dst = NamedTempFile::new_in(".").unwrap();

        assert_eq!(
            generate_downscaled_raster(src.path(), dst.path()).unwrap(),
            (2, 2)
        );
        let (width, height, rgba) = read_all(dst.path());
        assert_eq!((width, height), (2, 2));
        assert_eq!(&rgba[0..4], &[5, 5, 0, 255]);
        assert_eq!(&rgba[12..16], &[25, 25, 0, 255]);
    }

    #[test]
    fn test_box_odd_edges_3x2() {
        let src = make_bmp(5, 3, 3, |x, y| [(x * 10) as u8, (y * 20) as u8, 0, 255]);
        let dst = NamedTempFile::new_in(".").unwrap();

        assert_eq!(
            generate_downscaled_raster(src.path(), dst.path()).unwrap(),
            (3, 2)
        );
        let (_, _, rgba) = read_all(dst.path());
        let right_bottom = (1 * 3 + 2) * 4;
        assert_eq!(&rgba[right_bottom..right_bottom + 4], &[40, 40, 0, 255]);
    }

    #[test]
    fn test_box_32bit_premultiplied_alpha() {
        let src = make_bmp(2, 2, 4, |x, y| {
            if x == 0 && y == 0 {
                [200, 100, 50, 255]
            } else {
                [10, 240, 180, 0]
            }
        });
        let dst = NamedTempFile::new_in(".").unwrap();

        generate_downscaled_raster(src.path(), dst.path()).unwrap();
        let (_, _, rgba) = read_all(dst.path());
        assert_eq!(&rgba[0..4], &[199, 99, 51, 64]);
    }
}
