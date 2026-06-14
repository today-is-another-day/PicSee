use image::DynamicImage;
use lcms2::{Flags, Intent, PixelFormat, Profile, Transform};

/// 使用嵌入 ICC profile 将 RGB/RGBA 像素原地转换为 sRGB。
///
/// profile 或 transform 创建失败时返回 false，且不会修改像素。
pub fn convert_to_srgb_in_place(pixels: &mut [u8], has_alpha: bool, icc: &[u8]) -> bool {
    let channels = if has_alpha { 4 } else { 3 };
    if icc.is_empty() || pixels.len() % channels != 0 {
        return false;
    }

    let Ok(input_profile) = Profile::new_icc(icc) else {
        return false;
    };
    let output_profile = Profile::new_srgb();
    let format = if has_alpha {
        PixelFormat::RGBA_8
    } else {
        PixelFormat::RGB_8
    };
    let flags = if has_alpha {
        Flags::COPY_ALPHA
    } else {
        Flags::default()
    };
    let Ok(transform) = Transform::new_flags(
        &input_profile,
        format,
        &output_profile,
        format,
        Intent::Perceptual,
        flags,
    ) else {
        return false;
    };

    transform.transform_in_place(pixels);
    true
}

/// 将已解码图像按嵌入 profile 转成供现有显示管线使用的 8-bit sRGB。
pub(crate) fn dynamic_image_to_srgb(img: DynamicImage, icc: &[u8]) -> DynamicImage {
    if img.color().has_alpha() {
        let mut rgba = img.into_rgba8();
        convert_to_srgb_in_place(rgba.as_mut(), true, icc);
        DynamicImage::ImageRgba8(rgba)
    } else {
        let mut rgb = img.into_rgb8();
        convert_to_srgb_in_place(rgb.as_mut(), false, icc);
        DynamicImage::ImageRgb8(rgb)
    }
}

#[cfg(test)]
mod tests {
    use super::convert_to_srgb_in_place;
    use lcms2::{CIExyY, CIExyYTRIPLE, Profile, ToneCurve};

    fn display_p3_icc() -> Vec<u8> {
        let white_point = CIExyY {
            x: 0.3127,
            y: 0.3290,
            Y: 1.0,
        };
        let primaries = CIExyYTRIPLE {
            Red: CIExyY {
                x: 0.680,
                y: 0.320,
                Y: 1.0,
            },
            Green: CIExyY {
                x: 0.265,
                y: 0.690,
                Y: 1.0,
            },
            Blue: CIExyY {
                x: 0.150,
                y: 0.060,
                Y: 1.0,
            },
        };
        let curve = ToneCurve::new(2.2);
        Profile::new_rgb(&white_point, &primaries, &[&curve, &curve, &curve])
            .unwrap()
            .icc()
            .unwrap()
    }

    #[test]
    fn converts_profiled_rgba_to_srgb_and_preserves_alpha() {
        let mut pixels = vec![200, 80, 40, 73];
        let before = pixels.clone();

        assert!(convert_to_srgb_in_place(
            &mut pixels,
            true,
            &display_p3_icc()
        ));
        assert_ne!(&pixels[..3], &before[..3]);
        assert!(
            pixels[0] > before[0] || pixels[1] < before[1] || pixels[2] < before[2],
            "Display P3 像素应向 sRGB 方向收敛"
        );
        assert_eq!(pixels[3], before[3]);
    }

    #[test]
    fn rejects_empty_profile_without_modifying_pixels() {
        let mut pixels = vec![10, 20, 30];
        let before = pixels.clone();

        assert!(!convert_to_srgb_in_place(&mut pixels, false, &[]));
        assert_eq!(pixels, before);
    }

    #[test]
    fn rejects_invalid_profile_without_modifying_pixels() {
        let mut pixels = vec![10, 20, 30, 40];
        let before = pixels.clone();

        assert!(!convert_to_srgb_in_place(
            &mut pixels,
            true,
            b"not an ICC profile"
        ));
        assert_eq!(pixels, before);
    }
}
