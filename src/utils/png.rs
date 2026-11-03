//! PNG 编码：保存和剪贴板均使用无 alpha 的 RGB 像素。
//!
//! PNG 的颜色类型不决定远端剪贴板格式：Remmina 根据宣告的 MIME 类型映射，
//! `image/png` 对应 PNG，`image/bmp` 才对应 CF_DIB。多格式宣告由 clipboard 处理。

use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder, RgbImage};

use super::Bitmap;

/// 编码成 RGB PNG（缺省原始尺寸）
pub fn encode(bitmap: &Bitmap) -> Result<Vec<u8>, String> {
    encode_scaled(bitmap, 0)
}

/// 编码成 RGB PNG；`max_side > 0` 时把最长边限制到该值（等比缩小）
pub fn encode_scaled(bitmap: &Bitmap, max_side: u32) -> Result<Vec<u8>, String> {
    encode_rgb(&scaled_rgb_image(bitmap, max_side)?)
}

/// 编码已经转换、缩放后的 RGB 图像。
pub fn encode_rgb(img: &RgbImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            ExtendedColorType::Rgb8,
        )
        .map_err(|e| format!("PNG 编码失败: {e}"))?;
    Ok(out)
}

pub fn scaled_rgb_image(bitmap: &Bitmap, max_side: u32) -> Result<RgbImage, String> {
    let img = to_rgb_image(bitmap)?;
    let img = if max_side > 0 {
        let (w, h) = (img.width(), img.height());
        let longest = w.max(h);
        if longest > max_side {
            let scale = max_side as f32 / longest as f32;
            let nw = ((w as f32 * scale).round() as u32).max(1);
            let nh = ((h as f32 * scale).round() as u32).max(1);
            image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Lanczos3)
        } else {
            img
        }
    } else {
        img
    };

    Ok(img)
}

/// RGBA → RGB（直接丢 alpha：截图内容都是不透明的）
pub fn to_rgb_image(bitmap: &Bitmap) -> Result<RgbImage, String> {
    let mut buf = Vec::with_capacity(bitmap.width * bitmap.height * 3);
    for px in bitmap.rgba.chunks_exact(4) {
        buf.extend_from_slice(&px[..3]);
    }
    RgbImage::from_raw(bitmap.width as u32, bitmap.height as u32, buf)
        .ok_or_else(|| "构造 RGB 图像失败（尺寸与数据长度不符）".to_string())
}
