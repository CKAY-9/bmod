//! Collision mesh generation: a single convex hull around the model.

use parry3d::math::Vector3;
use parry3d::transformation::try_convex_hull;

use crate::mesh::Mesh;

/// VPhysics handles big hulls poorly; keep the vertex count modest.
const MAX_HULL_VERTS: usize = 200;
/// Smallest allowed thickness of the fallback box, in Source units.
const MIN_BOX_EXTENT: f32 = 1.0;

/// Returns hull triangles (counter-clockwise, outward facing) in mesh space.
pub fn convex_hull(mesh: &Mesh) -> Vec<[[f32; 3]; 3]> {
    let points: Vec<Vector3> = mesh.points().map(|[x, y, z]| Vector3::new(x, y, z)).collect();
    let (min, max) = mesh.bounds();
    let diag = (0..3).map(|i| (max[i] - min[i]).powi(2)).sum::<f32>().sqrt();

    let mut hull = try_convex_hull(&points).ok();
    // Simplify oversized hulls by snapping hull points onto progressively coarser grids.
    let mut cell = diag / 64.0;
    while let Some((verts, _)) = &hull {
        if verts.len() <= MAX_HULL_VERTS || cell <= 0.0 {
            break;
        }
        let mut snapped: Vec<Vector3> = verts
            .iter()
            .map(|p| Vector3::new((p.x / cell).round() * cell, (p.y / cell).round() * cell, (p.z / cell).round() * cell))
            .collect();
        snapped.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)).then(a.z.total_cmp(&b.z)));
        snapped.dedup();
        hull = try_convex_hull(&snapped).ok();
        cell *= 1.5;
    }

    match hull {
        Some((verts, idx)) if idx.len() >= 4 => idx
            .iter()
            .map(|t| t.map(|i| {
                let p = verts[i as usize];
                [p.x, p.y, p.z]
            }))
            .collect(),
        // Flat or degenerate geometry (e.g. a single plane): use a padded bounding box.
        _ => bounding_box(min, max),
    }
}

fn bounding_box(mut min: [f32; 3], mut max: [f32; 3]) -> Vec<[[f32; 3]; 3]> {
    for i in 0..3 {
        let pad = (MIN_BOX_EXTENT - (max[i] - min[i])).max(0.0) / 2.0;
        min[i] -= pad;
        max[i] += pad;
    }
    let c = |x: bool, y: bool, z: bool| {
        [if x { max[0] } else { min[0] }, if y { max[1] } else { min[1] }, if z { max[2] } else { min[2] }]
    };
    // Each face as a CCW quad seen from outside, split into two triangles.
    let quads = [
        [c(true, false, false), c(true, true, false), c(true, true, true), c(true, false, true)],   // +X
        [c(false, true, false), c(false, false, false), c(false, false, true), c(false, true, true)], // -X
        [c(true, true, false), c(false, true, false), c(false, true, true), c(true, true, true)],   // +Y
        [c(false, false, false), c(true, false, false), c(true, false, true), c(false, false, true)], // -Y
        [c(false, false, true), c(true, false, true), c(true, true, true), c(false, true, true)],   // +Z
        [c(false, true, false), c(true, true, false), c(true, false, false), c(false, false, false)], // -Z
    ];
    quads.iter().flat_map(|q| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::Vertex;

    fn tri(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> [Vertex; 3] {
        [a, b, c].map(|pos| Vertex { pos, normal: [0.0, 0.0, 1.0], uv: [0.0; 2] })
    }

    #[test]
    fn flat_mesh_falls_back_to_box() {
        let mesh = Mesh { triangles: vec![tri([0.0; 3], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0])] };
        let hull = convex_hull(&mesh);
        assert_eq!(hull.len(), 12);
        let zs: Vec<f32> = hull.iter().flatten().map(|p| p[2]).collect();
        assert!(zs.iter().any(|&z| z == -0.5) && zs.iter().any(|&z| z == 0.5));
    }

    #[test]
    fn hull_faces_point_outward() {
        let mesh = Mesh {
            triangles: vec![
                tri([0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
                tri([0.0; 3], [0.0, 0.0, 1.0], [1.0, 1.0, 1.0]),
            ],
        };
        let hull = convex_hull(&mesh);
        let centroid = glam::Vec3::splat(0.4);
        for t in hull {
            let [a, b, c] = t.map(glam::Vec3::from);
            assert!((b - a).cross(c - a).dot(a - centroid) > 0.0);
        }
    }
}
