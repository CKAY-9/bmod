//! Valve SMD writer for single-bone static meshes.

use std::fmt::Write as _;

use crate::collision::Triangle;
use crate::mesh::Mesh;

const HEADER: &str = "version 1\nnodes\n0 \"root\" -1\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\nend\ntriangles\n";

/// Appends `v` as `{:.6}` would, except that zero is never written as "-0.000000".
///
/// Formats through integers to skip the general float formatter, which is most
/// of the time spent writing a large SMD.
fn push_f(out: &mut String, v: f32) {
    // An f32 has 24 significant bits and 10^6 needs 20, so this product is exact
    // in f64 and rounding it is the same as rounding the decimal expansion.
    let scaled = (v as f64 * 1e6).round_ties_even();
    if scaled.is_nan() || scaled.abs() >= 1e15 {
        let _ = write!(out, "{:.6}", v + 0.0); // + 0.0 turns -0.0 into 0.0
        return;
    }
    let n = scaled as i64;
    if v.is_sign_negative() && v != 0.0 {
        out.push('-'); // also for values that round to zero, like `{:.6}`
    }
    let n = n.unsigned_abs();
    let _ = write!(out, "{}.{:06}", n / 1_000_000, n % 1_000_000);
}

/// Appends one SMD vertex line for the single bone `0`.
fn push_vertex(out: &mut String, pos: [f32; 3], normal: [f32; 3], uv: [f32; 2]) {
    out.push('0');
    for c in pos.into_iter().chain(normal).chain(uv) {
        out.push(' ');
        push_f(out, c);
    }
    out.push('\n');
}

/// Reference (visible) mesh; each triangle names its own material slot.
pub fn reference(mesh: &Mesh) -> String {
    let mut out = String::with_capacity(HEADER.len() + mesh.triangles.len() * 250);
    out.push_str(HEADER);
    for (tri, &mat) in mesh.triangles.iter().zip(&mesh.tri_material) {
        out.push_str(&mesh.materials[mat as usize]);
        out.push('\n');
        for v in tri {
            push_vertex(&mut out, v.pos, v.normal, v.uv);
        }
    }
    out.push_str("end\n");
    out
}

/// Physics mesh from closed convex pieces; UVs unused.
///
/// studiomdl finds the pieces of a `$concave` model by welding identical
/// vertices (position *and* normal), so every corner gets one normal per piece,
/// pointing away from the piece's centre. Flat face normals would split each
/// face off into its own "piece".
pub fn physics(pieces: &[Vec<Triangle>]) -> String {
    let tris: usize = pieces.iter().map(Vec::len).sum();
    let mut out = String::with_capacity(HEADER.len() + tris * 180);
    out.push_str(HEADER);
    for piece in pieces {
        let pts = || piece.iter().flatten().map(|&p| glam::Vec3::from(p));
        let centre = pts().sum::<glam::Vec3>() / pts().count().max(1) as f32;
        for tri in piece {
            out.push_str("phys\n");
            for &p in tri {
                let n = (glam::Vec3::from(p) - centre).try_normalize().unwrap_or(glam::Vec3::Z);
                out.push('0');
                for c in p.into_iter().chain(n.to_array()) {
                    out.push(' ');
                    push_f(&mut out, c);
                }
                out.push_str(" 0 0\n");
            }
        }
    }
    out.push_str("end\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::Vertex;

    fn check_f(v: f32) {
        let mut s = String::new();
        push_f(&mut s, v);
        let expected = format!("{:.6}", if v == 0.0 { 0.0 } else { v });
        assert_eq!(s, expected, "for {v:e} ({:#x})", v.to_bits());
    }

    #[test]
    fn float_formatting_matches_std() {
        let special = [
            0.0, -0.0, 1.0, -1.0, 0.5, 1e-7, -1e-7, 4e-7, 5e-7, 6e-7, -5e-7, 0.0078125, -0.0078125,
            1.5e-6, 2.5e-6, 0.123_456_5, 39.37, -1234.5678, 99_999.99, 1e8, -3e9, 1e20, f32::MAX, f32::MIN_POSITIVE,
        ];
        special.into_iter().for_each(check_f);
        // Exact ties at the 7th decimal: odd multiples of 2^-7 (= 0.0078125 = 15625e-6 / 2).
        for k in (1..2000).step_by(2) {
            check_f(k as f32 / 128.0);
            check_f(-(k as f32) / 128.0);
        }
        // A broad sweep of bit patterns across the useful range.
        let mut x: u32 = 0x9E37_79B9;
        for _ in 0..2_000_000 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let v = f32::from_bits(x);
            if v.is_finite() && v.abs() < 1e12 {
                check_f(v);
            }
            // And values typical of models, normals and UVs.
            for range in [1.0, 100.0, 10_000.0] {
                check_f((x as f32 / u32::MAX as f32 - 0.5) * range);
            }
        }
    }

    #[test]
    fn physics_corners_share_one_normal_per_piece() {
        let hull = crate::collision::convex_hull(&Mesh {
            triangles: vec![[[0.0; 3], [4.0, 0.0, 0.0], [0.0, 4.0, 0.0]].map(|pos| Vertex {
                pos,
                normal: [0.0, 0.0, 1.0],
                uv: [0.0; 2],
            })],
            ..Default::default()
        });
        let smd = physics(&[hull]);
        let mut seen = std::collections::HashMap::new();
        for line in smd.lines().filter(|l| l.starts_with("0 ") && l.split(' ').count() == 9) {
            let v: Vec<&str> = line.split(' ').collect();
            let normal = seen.entry(v[1..4].join(" ")).or_insert_with(|| v[4..7].join(" "));
            assert_eq!(*normal, v[4..7].join(" "), "corner {:?} has differing normals", &v[1..4]);
        }
        assert_eq!(seen.len(), 8);
    }

    #[test]
    fn writes_reference_smd() {
        let v = |x: f32| Vertex { pos: [x, 0.0, -0.0], normal: [0.0, 0.0, 1.0], uv: [0.5, 1.0] };
        let mut mesh = Mesh::default();
        let body = mesh.material_slot("Body");
        let money = mesh.material_slot("Money.001");
        mesh.push([v(0.0), v(1.0), v(2.0)], body, 0);
        mesh.push([v(3.0), v(4.0), v(5.0)], money, 0);
        let smd = reference(&mesh);
        assert!(smd.starts_with("version 1\nnodes\n"));
        assert!(smd.contains("triangles\nbody\n0 0.000000 0.000000 0.000000 0.000000 0.000000 1.000000 0.500000 1.000000\n"));
        assert!(smd.contains("\nmoney_001\n0 3.000000 "));
        assert!(smd.ends_with("end\n"));
        assert_eq!(smd.lines().filter(|l| l.starts_with("0 ") && l.contains('.')).count(), 6);
    }
}
