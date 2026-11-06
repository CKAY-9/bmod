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
}

impl Mesh {
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
