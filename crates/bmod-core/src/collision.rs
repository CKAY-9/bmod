//! Collision mesh generation: either one convex hull around the whole model,
//! or an approximate convex decomposition (V-HACD) that keeps holes and
//! hollows open, compiled by studiomdl as a `$concave` model.

use parry3d::math::Vector3;
use parry3d::transformation::try_convex_hull;
use parry3d::transformation::vhacd::{VHACD, VHACDParameters};

use crate::mesh::Mesh;

pub type Triangle = [[f32; 3]; 3];

/// VPhysics handles big hulls poorly; keep the vertex count modest.
const MAX_HULL_VERTS: usize = 200;
/// Per-piece vertex budget for decompositions, which have many pieces.
const MAX_PIECE_VERTS: usize = 64;
/// Upper bound on convex pieces; studiomdl/VPhysics get slow and unstable beyond this.
pub const MAX_CONVEX_PIECES: u32 = 32;
/// Voxel grid resolution along the longest axis for the decomposition.
const VOXEL_RESOLUTION: u32 = 64;
/// How far each piece is pulled inward, in Source units, so neighbouring pieces
/// never share a vertex (studiomdl would weld them into one convex piece).
const PIECE_GAP: f32 = 0.02;
/// Smallest allowed thickness of the fallback box, in Source units.
const MIN_BOX_EXTENT: f32 = 1.0;

/// Returns hull triangles (counter-clockwise, outward facing) in mesh space.
pub fn convex_hull(mesh: &Mesh) -> Vec<Triangle> {
    let points: Vec<Vector3> = mesh.points().map(|[x, y, z]| Vector3::new(x, y, z)).collect();
    let (min, max) = mesh.bounds();
    // Flat or degenerate geometry (e.g. a single plane): use a padded bounding box.
    hull_triangles(&points, diagonal(min, max), MAX_HULL_VERTS).unwrap_or_else(|| bounding_box(min, max))
}

/// Splits the model into convex pieces that together follow its concave shape.
/// Each inner `Vec` is one closed convex piece. Falls back to a single
/// [`convex_hull`] if the decomposition produces nothing usable.
pub fn convex_decomposition(mesh: &Mesh) -> Vec<Vec<Triangle>> {
    let points: Vec<Vector3> = mesh.points().map(|[x, y, z]| Vector3::new(x, y, z)).collect();
    let indices: Vec<[u32; 3]> = (0..mesh.triangles.len() as u32).map(|i| [3 * i, 3 * i + 1, 3 * i + 2]).collect();
    let (min, max) = mesh.bounds();
    let diag = diagonal(min, max);

    let params = VHACDParameters {
        resolution: VOXEL_RESOLUTION,
        concavity: 0.005,
        max_convex_hulls: MAX_CONVEX_PIECES,
        ..Default::default()
    };
    let vhacd = VHACD::decompose(&params, &points, &indices, true);
    let exact = vhacd.compute_exact_convex_hulls(&points, &indices);
    let voxel = vhacd.compute_convex_hulls(params.convex_hull_downsampling);

    let pieces: Vec<Vec<Triangle>> = exact
        .iter()
        .zip(&voxel)
        .filter_map(|((exact_pts, _), (voxel_pts, _))| {
            // Exact hulls hug the real surface; voxel hulls cover parts with no surface triangles.
            hull_triangles(exact_pts, diag, MAX_PIECE_VERTS)
                .or_else(|| hull_triangles(voxel_pts, diag, MAX_PIECE_VERTS))
        })
        .map(shrink)
        .collect();
    if pieces.is_empty() { vec![convex_hull(mesh)] } else { pieces }
}

fn diagonal(min: [f32; 3], max: [f32; 3]) -> f32 {
    (0..3).map(|i| (max[i] - min[i]).powi(2)).sum::<f32>().sqrt()
}

/// Convex hull of `points` with at most about `max_verts` vertices, as triangles.
/// `None` for flat or degenerate input.
fn hull_triangles(points: &[Vector3], diag: f32, max_verts: usize) -> Option<Vec<Triangle>> {
    let mut hull = try_convex_hull(points).ok();
    // Simplify oversized hulls by snapping hull points onto progressively coarser grids.
    let mut cell = diag / 64.0;
    while let Some((verts, _)) = &hull {
        if verts.len() <= max_verts || cell <= 0.0 {
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
        Some((verts, idx)) if idx.len() >= 4 => Some(
            idx.iter()
                .map(|t| t.map(|i| {
                    let p = verts[i as usize];
                    [p.x, p.y, p.z]
                }))
                .collect(),
        ),
        _ => None,
    }
}

/// Scales a convex piece about its centroid so it shrinks by about [`PIECE_GAP`].
fn shrink(piece: Vec<Triangle>) -> Vec<Triangle> {
    let pts = || piece.iter().flatten().map(|&p| glam::Vec3::from(p));
    let centroid = pts().sum::<glam::Vec3>() / (piece.len() * 3) as f32;
    let radius = pts().map(|p| p.distance(centroid)).fold(0.0, f32::max);
    if radius <= PIECE_GAP * 2.0 {
        return piece;
    }
    let s = 1.0 - PIECE_GAP / radius;
    piece
        .iter()
        .map(|t| t.map(|p| (centroid + (glam::Vec3::from(p) - centroid) * s).to_array()))
        .collect()
}

fn bounding_box(mut min: [f32; 3], mut max: [f32; 3]) -> Vec<Triangle> {
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
        let mesh = Mesh {
            triangles: vec![tri([0.0; 3], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0])],
            ..Default::default()
        };
        let hull = convex_hull(&mesh);
        assert_eq!(hull.len(), 12);
        let zs: Vec<f32> = hull.iter().flatten().map(|p| p[2]).collect();
        assert!(zs.iter().any(|&z| z == -0.5) && zs.iter().any(|&z| z == 0.5));
    }

    /// Closed axis-aligned box as 12 outward-facing triangles.
    fn cuboid(min: [f32; 3], max: [f32; 3]) -> Vec<[Vertex; 3]> {
        bounding_box(min, max).into_iter().map(|[a, b, c]| tri(a, b, c)).collect()
    }

    fn contains(piece: &[Triangle], p: glam::Vec3) -> bool {
        piece.iter().all(|t| {
            let [a, b, c] = t.map(glam::Vec3::from);
            (b - a).cross(c - a).dot(p - a) <= 0.0
        })
    }

    #[test]
    fn decomposition_keeps_cup_open() {
        // A 20x20x20 cup: 2-unit-thick bottom and walls, open at the top.
        let mut triangles = cuboid([0.0; 3], [20.0, 20.0, 2.0]);
        triangles.extend(cuboid([0.0, 0.0, 0.0], [2.0, 20.0, 20.0]));
        triangles.extend(cuboid([18.0, 0.0, 0.0], [20.0, 20.0, 20.0]));
        triangles.extend(cuboid([0.0, 0.0, 0.0], [20.0, 2.0, 20.0]));
        triangles.extend(cuboid([0.0, 18.0, 0.0], [20.0, 20.0, 20.0]));
        let mesh = Mesh { tri_material: vec![0; triangles.len()], triangles, materials: vec!["m".into()] };

        let pieces = convex_decomposition(&mesh);
        assert!(pieces.len() > 1, "expected several pieces, got {}", pieces.len());
        assert!(pieces.len() <= MAX_CONVEX_PIECES as usize);
        for z in [5.0, 10.0, 15.0, 19.0] {
            let inside = glam::Vec3::new(10.0, 10.0, z);
            assert!(!pieces.iter().any(|p| contains(p, inside)), "cavity filled at z={z}");
        }
        // The walls and bottom themselves are still solid (points kept off the split planes).
        for p in [[1.0, 6.0, 10.0], [14.0, 19.0, 10.0], [6.0, 6.0, 1.0], [14.0, 14.0, 1.0]] {
            let p = glam::Vec3::from(p);
            assert!(pieces.iter().any(|piece| contains(piece, p)), "wall missing at {p:?}");
        }
    }

    #[test]
    fn hull_faces_point_outward() {
        let mesh = Mesh {
            triangles: vec![
                tri([0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
                tri([0.0; 3], [0.0, 0.0, 1.0], [1.0, 1.0, 1.0]),
            ],
            ..Default::default()
        };
        let hull = convex_hull(&mesh);
        let centroid = glam::Vec3::splat(0.4);
        for t in hull {
            let [a, b, c] = t.map(glam::Vec3::from);
            assert!((b - a).cross(c - a).dot(a - centroid) > 0.0);
        }
    }
}
