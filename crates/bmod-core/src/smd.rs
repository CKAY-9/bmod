//! Valve SMD writer for single-bone static meshes.

use std::fmt::Write as _;

use crate::collision::Triangle;
use crate::mesh::Mesh;

const HEADER: &str = "version 1\nnodes\n0 \"root\" -1\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\nend\ntriangles\n";

fn f(v: f32) -> String {
    // Avoid "-0.000000" noise in the output.
    let v = if v == 0.0 { 0.0 } else { v };
    format!("{v:.6}")
}

/// Reference (visible) mesh; each triangle names its own material slot.
pub fn reference(mesh: &Mesh) -> String {
    let mut out = String::with_capacity(HEADER.len() + mesh.triangles.len() * 200);
    out.push_str(HEADER);
    for (tri, &mat) in mesh.triangles.iter().zip(&mesh.tri_material) {
        out.push_str(&mesh.materials[mat as usize]);
        out.push('\n');
        for v in tri {
            let _ = writeln!(
                out,
                "0 {} {} {} {} {} {} {} {}",
                f(v.pos[0]), f(v.pos[1]), f(v.pos[2]),
                f(v.normal[0]), f(v.normal[1]), f(v.normal[2]),
                f(v.uv[0]), f(v.uv[1]),
            );
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
                let _ = writeln!(
                    out,
                    "0 {} {} {} {} {} {} 0 0",
                    f(p[0]), f(p[1]), f(p[2]), f(n.x), f(n.y), f(n.z),
                );
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
        mesh.push([v(0.0), v(1.0), v(2.0)], body);
        mesh.push([v(3.0), v(4.0), v(5.0)], money);
        let smd = reference(&mesh);
        assert!(smd.starts_with("version 1\nnodes\n"));
        assert!(smd.contains("triangles\nbody\n0 0.000000 0.000000 0.000000 0.000000 0.000000 1.000000 0.500000 1.000000\n"));
        assert!(smd.contains("\nmoney_001\n0 3.000000 "));
        assert!(smd.ends_with("end\n"));
        assert_eq!(smd.lines().filter(|l| l.starts_with("0 ") && l.contains('.')).count(), 6);
    }
}
