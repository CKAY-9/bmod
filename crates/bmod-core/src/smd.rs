//! Valve SMD writer for single-bone static meshes.

use std::fmt::Write as _;

use crate::mesh::Mesh;

const HEADER: &str = "version 1\nnodes\n0 \"root\" -1\nend\nskeleton\ntime 0\n0 0 0 0 0 0 0\nend\ntriangles\n";

fn f(v: f32) -> String {
    // Avoid "-0.000000" noise in the output.
    let v = if v == 0.0 { 0.0 } else { v };
    format!("{v:.6}")
}

/// Reference (visible) mesh; every triangle uses `material`.
pub fn reference(mesh: &Mesh, material: &str) -> String {
    let mut out = String::with_capacity(HEADER.len() + mesh.triangles.len() * 200);
    out.push_str(HEADER);
    for tri in &mesh.triangles {
        out.push_str(material);
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

/// Physics mesh from bare triangles. Normals are flat per face; UVs unused.
pub fn physics(triangles: &[[[f32; 3]; 3]]) -> String {
    let mut out = String::with_capacity(HEADER.len() + triangles.len() * 180);
    out.push_str(HEADER);
    for tri in triangles {
        let [a, b, c] = tri.map(glam::Vec3::from);
        let n = (b - a).cross(c - a).try_normalize().unwrap_or(glam::Vec3::Z);
        out.push_str("phys\n");
        for p in tri {
            let _ = writeln!(
                out,
                "0 {} {} {} {} {} {} 0 0",
                f(p[0]), f(p[1]), f(p[2]), f(n.x), f(n.y), f(n.z),
            );
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
    fn writes_reference_smd() {
        let v = |x: f32| Vertex { pos: [x, 0.0, -0.0], normal: [0.0, 0.0, 1.0], uv: [0.5, 1.0] };
        let mesh = Mesh { triangles: vec![[v(0.0), v(1.0), v(2.0)]] };
        let smd = reference(&mesh, "crate");
        assert!(smd.starts_with("version 1\nnodes\n"));
        assert!(smd.contains("triangles\ncrate\n0 0.000000 0.000000 0.000000 0.000000 0.000000 1.000000 0.500000 1.000000\n"));
        assert!(smd.ends_with("end\n"));
        assert_eq!(smd.lines().filter(|l| l.starts_with("0 ") && l.contains('.')).count(), 3);
    }
}
