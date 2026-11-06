//! Minimal VTF 7.2 writer: DXT1 (opaque) or DXT5 (with alpha), full mip chain,
//! DXT1 low-res thumbnail.
//!
//! Layout: 80-byte header, low-res image, then mipmaps from smallest to largest.

use image::{DynamicImage, RgbaImage, imageops::FilterType};
use texpresso::{Format, Params};

/// Source supports up to 4096; larger textures are downscaled.
pub const MAX_SIZE: u32 = 4096;

const HEADER_SIZE: usize = 80;
const FMT_DXT1: u32 = 13;
const FMT_DXT5: u32 = 15;
const FLAG_EIGHTBITALPHA: u32 = 0x2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VtfFormat {
    Dxt1,
    Dxt5,
}

#[derive(Debug)]
pub struct EncodedVtf {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub resized_from: Option<(u32, u32)>,
    pub format: VtfFormat,
    pub mip_count: u8,
}

/// Rounds to the nearest power of two (ties go up), clamped to `1..=MAX_SIZE`.
pub fn nearest_pow2(n: u32) -> u32 {
    let n = n.clamp(1, MAX_SIZE);
    let up = n.next_power_of_two();
    let down = up / 2;
    if down >= 1 && n - down < up - n { down } else { up }.min(MAX_SIZE)
}

pub fn encode(img: &DynamicImage) -> EncodedVtf {
    let src = img.to_rgba8();
    let (w, h) = (nearest_pow2(src.width()), nearest_pow2(src.height()));
    let resized_from = ((w, h) != src.dimensions()).then(|| src.dimensions());
    let base = if resized_from.is_some() {
        image::imageops::resize(&src, w, h, FilterType::Lanczos3)
    } else {
        src
    };

    let has_alpha = base.pixels().any(|p| p.0[3] < 255);
    let (format, fmt_id, flags) = if has_alpha {
        (Format::Bc3, FMT_DXT5, FLAG_EIGHTBITALPHA)
    } else {
        (Format::Bc1, FMT_DXT1, 0)
    };

    // Mip chain, largest first.
    let mut mips = vec![base];
    while let Some(last) = mips.last().filter(|m| m.width() > 1 || m.height() > 1) {
        let (mw, mh) = ((last.width() / 2).max(1), (last.height() / 2).max(1));
        let next = image::imageops::resize(last, mw, mh, FilterType::Triangle);
        mips.push(next);
    }

    let thumb_scale = 16.0 / w.max(h) as f32;
    let (tw, th) = if thumb_scale < 1.0 {
        (((w as f32 * thumb_scale) as u32).max(1), ((h as f32 * thumb_scale) as u32).max(1))
    } else {
        (w, h)
    };
    let thumb = image::imageops::resize(&mips[0], tw, th, FilterType::Triangle);
    let reflectivity = reflectivity(&thumb);

    let mut out = Vec::with_capacity(HEADER_SIZE + mips.iter().map(|m| (m.width() * m.height()) as usize).sum::<usize>());
    out.extend_from_slice(b"VTF\0");
    out.extend_from_slice(&7u32.to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(HEADER_SIZE as u32).to_le_bytes());
    out.extend_from_slice(&(w as u16).to_le_bytes());
    out.extend_from_slice(&(h as u16).to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // frames
    out.extend_from_slice(&0u16.to_le_bytes()); // first frame
    out.extend_from_slice(&[0; 4]); // padding
    for c in reflectivity {
        out.extend_from_slice(&c.to_le_bytes());
    }
    out.extend_from_slice(&[0; 4]); // padding
    out.extend_from_slice(&1.0f32.to_le_bytes()); // bumpmap scale
    out.extend_from_slice(&fmt_id.to_le_bytes());
    out.push(mips.len() as u8);
    out.extend_from_slice(&FMT_DXT1.to_le_bytes()); // low-res format
    out.push(tw as u8);
    out.push(th as u8);
    out.extend_from_slice(&1u16.to_le_bytes()); // depth
    debug_assert_eq!(out.len(), 65);
    out.resize(HEADER_SIZE, 0);

    compress_into(&mut out, &thumb, Format::Bc1);
    for mip in mips.iter().rev() {
        compress_into(&mut out, mip, format);
    }

    EncodedVtf {
        bytes: out,
        width: w,
        height: h,
        resized_from,
        format: if has_alpha { VtfFormat::Dxt5 } else { VtfFormat::Dxt1 },
        mip_count: mips.len() as u8,
    }
}

fn compress_into(out: &mut Vec<u8>, img: &RgbaImage, format: Format) {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let start = out.len();
    out.resize(start + format.compressed_size(w, h), 0);
    let params = Params { weigh_colour_by_alpha: format == Format::Bc3, ..Default::default() };
    format.compress(img.as_raw(), w, h, params, &mut out[start..]);
}

/// Average linear colour, used by VRAD for bounce lighting.
fn reflectivity(img: &RgbaImage) -> [f32; 3] {
    let mut sum = [0.0f64; 3];
    for p in img.pixels() {
        for (s, &c) in sum.iter_mut().zip(&p.0[..3]) {
            *s += (c as f64 / 255.0).powf(2.2);
        }
    }
    let n = (img.width() * img.height()).max(1) as f64;
    sum.map(|s| (s / n) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pow2_rounding() {
        assert_eq!(nearest_pow2(1), 1);
        assert_eq!(nearest_pow2(512), 512);
        assert_eq!(nearest_pow2(700), 512);
        assert_eq!(nearest_pow2(800), 1024);
        assert_eq!(nearest_pow2(768), 1024);
        assert_eq!(nearest_pow2(10_000), 4096);
    }

    #[test]
    fn encodes_opaque_dxt1() {
        let img = DynamicImage::ImageRgba8(RgbaImage::from_pixel(300, 64, image::Rgba([255, 0, 0, 255])));
        let vtf = encode(&img);
        assert_eq!((vtf.width, vtf.height), (256, 64));
        assert_eq!(vtf.resized_from, Some((300, 64)));
        assert_eq!(vtf.format, VtfFormat::Dxt1);
        assert_eq!(vtf.mip_count, 9); // 256x64 .. 1x1
        let b = &vtf.bytes;
        assert_eq!(&b[0..4], b"VTF\0");
        assert_eq!(u32::from_le_bytes(b[12..16].try_into().unwrap()), 80);
        assert_eq!(u16::from_le_bytes(b[16..18].try_into().unwrap()), 256);
        assert_eq!(u32::from_le_bytes(b[52..56].try_into().unwrap()), FMT_DXT1);
        assert_eq!((b[61], b[62]), (16, 4)); // thumbnail keeps aspect ratio

        // Size = header + thumb + every mip, all DXT1 (8 bytes per 4x4 block).
        let blocks = |w: u32, h: u32| (w.div_ceil(4) * h.div_ceil(4)) as usize * 8;
        let mut expected = 80 + blocks(16, 4);
        let (mut w, mut h) = (256, 64);
        loop {
            expected += blocks(w, h);
            if w == 1 && h == 1 { break; }
            (w, h) = ((w / 2).max(1), (h / 2).max(1));
        }
        assert_eq!(b.len(), expected);
        // Reflectivity of pure red.
        assert!((f32::from_le_bytes(b[32..36].try_into().unwrap()) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn alpha_selects_dxt5() {
        let img = DynamicImage::ImageRgba8(RgbaImage::from_pixel(64, 64, image::Rgba([0, 0, 0, 128])));
        let vtf = encode(&img);
        assert_eq!(vtf.format, VtfFormat::Dxt5);
        assert_eq!(u32::from_le_bytes(vtf.bytes[20..24].try_into().unwrap()), FLAG_EIGHTBITALPHA);
    }
}
