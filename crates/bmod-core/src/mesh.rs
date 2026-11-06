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
