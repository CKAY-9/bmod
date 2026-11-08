//! Minimal VTF 7.2 writer: DXT1 (opaque) or DXT5 (with alpha), full mip chain,
//! DXT1 low-res thumbnail.
//!
//! Layout: 80-byte header, low-res image, then mipmaps from smallest to largest.

use image::{DynamicImage, RgbaImage, imageops::FilterType};
use texpresso::{Algorithm, Format, Params};

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

/// Speed/quality trade-off for encoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextureQuality {
    /// Cluster-fit DXT compression and a Lanczos3 resize.
    #[default]
    High,
    /// Range-fit DXT compression (about 40x faster, with more blocky banding on
    /// gradients and fine detail) and a cheaper resize filter.
    Fast,
}

impl TextureQuality {
    fn algorithm(self) -> Algorithm {
        match self {
            TextureQuality::High => Algorithm::ClusterFit,
            TextureQuality::Fast => Algorithm::RangeFit,
        }
    }

    fn resize_filter(self) -> FilterType {
        match self {
            TextureQuality::High => FilterType::Lanczos3,
            TextureQuality::Fast => FilterType::Triangle,
        }
    }
}

/// What a VTF written by [`encode`] contains, as read back from its header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VtfInfo {
    pub width: u32,
    pub height: u32,
    pub format: VtfFormat,
    pub mip_count: u8,
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

pub fn encode(img: &DynamicImage, quality: TextureQuality) -> EncodedVtf {
    let src = img.to_rgba8();
    let (w, h) = (nearest_pow2(src.width()), nearest_pow2(src.height()));
    let resized_from = ((w, h) != src.dimensions()).then(|| src.dimensions());
    let base = if resized_from.is_some() {
        image::imageops::resize(&src, w, h, quality.resize_filter())
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

    // The low-res thumbnail is at most 16 px on its long side, keeping the aspect
    // ratio. Both sides are powers of two, so that is exactly one of the mips.
    let thumb = &mips[(w.max(h) / 16).max(1).trailing_zeros() as usize];
    let (tw, th) = thumb.dimensions();
    let reflectivity = reflectivity(thumb);

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

    let algorithm = quality.algorithm();
    compress_into(&mut out, thumb, Format::Bc1, algorithm);
    for mip in mips.iter().rev() {
        compress_into(&mut out, mip, format, algorithm);
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

/// Reads back the header of a VTF written by [`encode`]. `None` if `bytes` isn't
/// one, or is truncated or padded.
pub fn read_info(bytes: &[u8]) -> Option<VtfInfo> {
    if bytes.len() < HEADER_SIZE || &bytes[0..4] != b"VTF\0" {
        return None;
    }
    let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]) as u32;
    let u32_at = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
    if (u32_at(4), u32_at(8), u32_at(12)) != (7, 2, HEADER_SIZE as u32) {
        return None;
    }
    let (width, height) = (u16_at(16), u16_at(18));
    let (format, block) = match u32_at(52) {
        FMT_DXT1 => (VtfFormat::Dxt1, Format::Bc1),
        FMT_DXT5 => (VtfFormat::Dxt5, Format::Bc3),
        _ => return None,
    };
    let mip_count = bytes[56];
    let (tw, th) = (bytes[61] as usize, bytes[62] as usize);
    let mut expected = HEADER_SIZE + Format::Bc1.compressed_size(tw, th);
    for level in 0..mip_count as u32 {
        let (mw, mh) = ((width >> level).max(1), (height >> level).max(1));
        expected += block.compressed_size(mw as usize, mh as usize);
    }
    (width > 0 && height > 0 && expected == bytes.len()).then_some(VtfInfo { width, height, format, mip_count })
}

fn compress_into(out: &mut Vec<u8>, img: &RgbaImage, format: Format, algorithm: Algorithm) {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let start = out.len();
    out.resize(start + format.compressed_size(w, h), 0);
    let params = Params { algorithm, weigh_colour_by_alpha: format == Format::Bc3, ..Default::default() };
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
        let vtf = encode(&img, TextureQuality::High);
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
    fn reads_back_info() {
        let img = DynamicImage::ImageRgba8(RgbaImage::from_fn(64, 32, |x, y| image::Rgba([x as u8 * 4, y as u8 * 8, 0, 200])));
        for quality in [TextureQuality::High, TextureQuality::Fast] {
            let vtf = encode(&img, quality);
            let info = read_info(&vtf.bytes).unwrap();
            assert_eq!(info, VtfInfo { width: 64, height: 32, format: VtfFormat::Dxt5, mip_count: vtf.mip_count });
            assert_eq!(read_info(&vtf.bytes[..vtf.bytes.len() - 1]), None, "truncated file accepted");
        }
        assert_eq!(read_info(b"not a vtf"), None);
    }

    #[test]
    fn alpha_selects_dxt5() {
        let img = DynamicImage::ImageRgba8(RgbaImage::from_pixel(64, 64, image::Rgba([0, 0, 0, 128])));
        let vtf = encode(&img, TextureQuality::High);
        assert_eq!(vtf.format, VtfFormat::Dxt5);
        assert_eq!(u32::from_le_bytes(vtf.bytes[20..24].try_into().unwrap()), FLAG_EIGHTBITALPHA);
    }
}
