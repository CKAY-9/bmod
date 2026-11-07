//! In-memory triangle soup, already in Source coordinate space (Z-up, +X forward).

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    /// UV with a bottom-left origin (Blender/OpenGL/SMD convention).
    pub uv: [f32; 2],
}

#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub triangles: Vec<[Vertex; 3]>,
    /// Material slot of each triangle; indexes `materials`. Same length as `triangles`.
    pub tri_material: Vec<u32>,
    /// Material slot names, already sanitized (`[a-z0-9_]`) and unique.
    pub materials: Vec<String>,
}

impl Mesh {
    /// Returns the slot for a material name from the source file, creating it if new.
    pub fn material_slot(&mut self, raw_name: &str) -> u32 {
        let name = match crate::sanitize_name(raw_name) {
            n if n.is_empty() => "material".to_string(),
            n => n,
        };
        match self.materials.iter().position(|m| *m == name) {
            Some(i) => i as u32,
            None => {
                self.materials.push(name);
                (self.materials.len() - 1) as u32
            }
        }
    }

    pub fn push(&mut self, tri: [Vertex; 3], material: u32) {
        self.triangles.push(tri);
        self.tri_material.push(material);
    }

    /// Triangle count per material slot.
    pub fn material_triangle_counts(&self) -> Vec<usize> {
        let mut counts = vec![0; self.materials.len()];
        for &m in &self.tri_material {
            counts[m as usize] += 1;
        }
        counts
    }

    /// Drops material slots that no triangle uses and renumbers the rest.
    pub fn prune_unused_materials(&mut self) {
        let counts = self.material_triangle_counts();
        let mut remap = vec![0u32; counts.len()];
        let mut kept = Vec::new();
        for (i, name) in std::mem::take(&mut self.materials).into_iter().enumerate() {
            if counts[i] > 0 {
                remap[i] = kept.len() as u32;
                kept.push(name);
            }
        }
        self.materials = kept;
        for m in &mut self.tri_material {
            *m = remap[*m as usize];
        }
    }

    pub fn scale(&mut self, factor: f32) {
        for tri in &mut self.triangles {
            for v in tri {
                for c in &mut v.pos {
                    *c *= factor;
                }
            }
        }
    }

    /// Axis-aligned bounds as (min, max). Returns zeros for an empty mesh.
    pub fn bounds(&self) -> ([f32; 3], [f32; 3]) {
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for v in self.triangles.iter().flatten() {
            for i in 0..3 {
                min[i] = min[i].min(v.pos[i]);
                max[i] = max[i].max(v.pos[i]);
            }
        }
        if self.triangles.is_empty() {
            return ([0.0; 3], [0.0; 3]);
        }
        (min, max)
    }

    pub fn points(&self) -> impl Iterator<Item = [f32; 3]> + '_ {
        self.triangles.iter().flatten().map(|v| v.pos)
    }

    pub fn is_finite(&self) -> bool {
        self.triangles.iter().flatten().all(|v| {
            v.pos.iter().chain(&v.normal).chain(&v.uv).all(|c| c.is_finite())
        })
    }
}

/// Area-weighted smooth normals for an indexed triangle list.
pub fn smooth_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut acc = vec![glam::Vec3::ZERO; positions.len()];
    for tri in indices.as_chunks::<3>().0 {
        // Bad indices are reported by the caller; just skip them here.
        if tri.iter().any(|&i| i as usize >= positions.len()) {
            continue;
        }
        let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| glam::Vec3::from(positions[i as usize]));
        let n = (b - a).cross(c - a);
        for &i in tri {
            acc[i as usize] += n;
        }
    }
    acc.into_iter()
        .map(|n| n.try_normalize().unwrap_or(glam::Vec3::Z).to_array())
        .collect()
}

/// Splits a planar polygon (corners in order, possibly concave) into triangles,
/// as index triples into `corners` with the polygon's winding preserved.
///
/// Uses ear clipping in the polygon's own plane. A naive fan from the first
/// corner only works for convex polygons; on concave ones it lays triangles
/// over openings (e.g. across the mouth of a cup). Self-intersecting leftovers
/// that have no ear fall back to a fan.
pub fn triangulate_polygon(corners: &[[f32; 3]]) -> Vec<[usize; 3]> {
    use glam::{DVec2, DVec3};

    let n = corners.len();
    if n < 3 {
        return Vec::new();
    }
    let fan = |idx: &[usize]| -> Vec<[usize; 3]> { (1..idx.len() - 1).map(|k| [idx[0], idx[k], idx[k + 1]]).collect() };
    let all: Vec<usize> = (0..n).collect();
    if n == 3 {
        return fan(&all);
    }

    // Project onto the plane of the Newell normal; the polygon is then counter-clockwise in 2D.
    let p3: Vec<DVec3> = corners.iter().map(|&c| glam::Vec3::from(c).as_dvec3()).collect();
    let normal: DVec3 = (0..n)
        .map(|i| {
            let (a, b) = (p3[i], p3[(i + 1) % n]);
            DVec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y))
        })
        .sum();
    let Some(normal) = normal.try_normalize() else { return fan(&all) };
    let u = normal.any_orthonormal_vector();
    let v = normal.cross(u);
    let p: Vec<DVec2> = p3.iter().map(|q| DVec2::new(q.dot(u), q.dot(v))).collect();

    let (lo, hi) = p.iter().fold((p[0], p[0]), |(lo, hi), q| (lo.min(*q), hi.max(*q)));
    let eps = (hi - lo).length_squared() * 1e-12;
    let cross = |o: DVec2, a: DVec2, b: DVec2| (a - o).perp_dot(b - o);

    let mut idx = all;
    let mut out = Vec::with_capacity(n - 2);
    while idx.len() > 3 {
        let m = idx.len();
        let corner = |k: usize| (idx[(k + m - 1) % m], idx[k], idx[(k + 1) % m]);
        let is_ear = |k: usize| {
            let (a, b, c) = corner(k);
            if cross(p[a], p[b], p[c]) <= eps {
                return false; // reflex or collinear
            }
            // No other corner may lie inside or on the candidate triangle. Corners at the
            // same position as the triangle's own corners (bridge seams) don't count.
            idx.iter().all(|&j| {
                j == a || j == b || j == c
                    || p[j] == p[a] || p[j] == p[b] || p[j] == p[c]
                    || cross(p[a], p[b], p[j]) < -eps
                    || cross(p[b], p[c], p[j]) < -eps
                    || cross(p[c], p[a], p[j]) < -eps
            })
        };
        if let Some(k) = (0..m).find(|&k| is_ear(k)) {
            let (a, b, c) = corner(k);
            out.push([a, b, c]);
            idx.remove(k);
        } else if let Some(k) = (0..m).find(|&k| {
            let (a, b, c) = corner(k);
            cross(p[a], p[b], p[c]).abs() <= eps
        }) {
            // A collinear corner adds no area; drop it.
            idx.remove(k);
        } else {
            out.extend(fan(&idx));
            return out;
        }
    }
    let (a, b, c) = (idx[0], idx[1], idx[2]);
    if cross(p[a], p[b], p[c]).abs() > eps {
        out.push([a, b, c]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Twice the signed area of each triangle in the XY plane.
    fn areas(pts: &[[f32; 3]], tris: &[[usize; 3]]) -> Vec<f32> {
        tris.iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| glam::Vec2::new(pts[i][0], pts[i][1]));
                (b - a).perp_dot(c - a)
            })
            .collect()
    }

    #[test]
    fn triangulates_concave_polygon_without_covering_the_notch() {
        // A "U": the notch between the arms (around x=2, y=3) must stay empty.
        let u = [
            [0.0, 0.0, 0.0], [4.0, 0.0, 0.0], [4.0, 4.0, 0.0], [3.0, 4.0, 0.0],
            [3.0, 1.0, 0.0], [1.0, 1.0, 0.0], [1.0, 4.0, 0.0], [0.0, 4.0, 0.0],
        ];
        let tris = triangulate_polygon(&u);
        assert_eq!(tris.len(), u.len() - 2);
        let a = areas(&u, &tris);
        assert!(a.iter().all(|&x| x > 0.0), "winding flipped: {a:?}");
        assert!((a.iter().sum::<f32>() / 2.0 - 10.0).abs() < 1e-4, "area should be 10");
        for t in &tris {
            let [a, b, c] = t.map(|i| glam::Vec2::new(u[i][0], u[i][1]));
            let p = glam::Vec2::new(2.0, 3.0);
            let inside = (b - a).perp_dot(p - a) > 0.0 && (c - b).perp_dot(p - b) > 0.0 && (a - c).perp_dot(p - c) > 0.0;
            assert!(!inside, "triangle {t:?} covers the notch");
        }
    }

    #[test]
    fn triangulates_ring_with_bridge_seam() {
        // Square ring as one polygon: outer loop CCW, seam, inner loop CW, seam back.
        let ring = [
            [0.0, 0.0, 0.0], [4.0, 0.0, 0.0], [4.0, 4.0, 0.0], [0.0, 4.0, 0.0], [0.0, 0.0, 0.0],
            [1.0, 1.0, 0.0], [1.0, 3.0, 0.0], [3.0, 3.0, 0.0], [3.0, 1.0, 0.0], [1.0, 1.0, 0.0],
        ];
        let tris = triangulate_polygon(&ring);
        let a = areas(&ring, &tris);
        assert!(a.iter().all(|&x| x > 0.0), "winding flipped: {a:?}");
        assert!((a.iter().sum::<f32>() / 2.0 - 12.0).abs() < 1e-4, "area should be 16 - 4");
    }

    #[test]
    fn keeps_winding_of_tilted_clockwise_polygon() {
        // Clockwise when seen from +Z, tilted out of every axis plane.
        let quad: Vec<[f32; 3]> = [[0.0, 0.0], [0.0, 2.0], [1.0, 1.0], [2.0, 2.0], [2.0, 0.0]]
            .iter()
            .map(|&[x, y]| [x, y + 0.3 * x, 0.5 * y])
            .collect();
        let tris = triangulate_polygon(&quad);
        assert_eq!(tris.len(), 3);
        let n = |t: &[usize; 3]| {
            let [a, b, c] = t.map(|i| glam::Vec3::from(quad[i]));
            (b - a).cross(c - a)
        };
        assert!(tris.iter().all(|t| n(t).z < 0.0), "every triangle must keep the clockwise winding");
    }
}
