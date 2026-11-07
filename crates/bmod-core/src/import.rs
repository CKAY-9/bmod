//! Model loading. Every loader returns a [`Mesh`] in Source space, still in
//! the file's own units (metres for Blender exports); scaling happens later.
//!
//! glTF and Blender's OBJ exporter are both Y-up with -Z forward, which maps
//! onto Source/Blender space as `(x, y, z) -> (x, -z, y)`.

use std::path::Path;

use anyhow::{Context, Result, bail};
use glam::{Mat3, Mat4, Vec3};

use crate::Logger;
use crate::mesh::{Mesh, Vertex, smooth_normals, triangulate_polygon};

fn y_up_to_source(v: Vec3) -> [f32; 3] {
    [v.x, -v.z, v.y]
}

pub fn load_model(path: &Path, log: Logger) -> Result<Mesh> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let mut mesh = match ext.as_str() {
        "glb" | "gltf" => load_gltf(path, log)?,
        "obj" => load_obj(path, log)?,
        other => bail!(
            "unsupported model format '.{other}' (supported: {})",
            crate::MODEL_EXTENSIONS.join(", ")
        ),
    };
    if mesh.triangles.is_empty() {
        bail!("{} contains no triangles", path.display());
    }
    mesh.prune_unused();
    if !mesh.is_finite() {
        bail!("{} contains invalid (NaN/infinite) vertex data", path.display());
    }
    Ok(mesh)
}

fn load_gltf(path: &Path, log: Logger) -> Result<Mesh> {
    let (doc, buffers, _) =
        gltf::import(path).with_context(|| format!("failed to read glTF {}", path.display()))?;
    let scene = doc
        .default_scene()
        .or_else(|| doc.scenes().next())
        .context("glTF file has no scenes")?;

    let mut mesh = Mesh::default();
    let mut missing_uvs = false;
    // Depth-first in file order (pushed reversed), so slots and parts list in the outliner's order.
    let mut stack: Vec<(gltf::Node, Mat4)> = scene.nodes().map(|n| (n, Mat4::IDENTITY)).collect();
    stack.reverse();

    while let Some((node, parent)) = stack.pop() {
        let world = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
        let first_child = stack.len();
        stack.extend(node.children().map(|c| (c, world)));
        stack[first_child..].reverse();

        let Some(gmesh) = node.mesh() else { continue };
        // Blender names the node after the object and the mesh after its data block.
        let part = match (node.name(), gmesh.name()) {
            (Some(name), _) | (None, Some(name)) => mesh.part_slot(name),
            (None, None) => mesh.part_slot(&format!("part_{}", node.index())),
        };
        let normal_mat = Mat3::from_mat4(world).inverse().transpose();
        let mirrored = world.determinant() < 0.0;

        for prim in gmesh.primitives() {
            if prim.mode() != gltf::mesh::Mode::Triangles {
                log(&format!(
                    "warning: skipping non-triangle primitive in mesh '{}'",
                    gmesh.name().unwrap_or("?")
                ));
                continue;
            }
            let gmat = prim.material();
            let material = match (gmat.name(), gmat.index()) {
                (Some(name), _) => mesh.material_slot(name),
                (None, Some(i)) => mesh.material_slot(&format!("material_{i}")),
                (None, None) => mesh.material_slot("default"),
            };
            let reader = prim.reader(|b| Some(&buffers[b.index()]));
            let Some(positions) = reader.read_positions() else { continue };
            let positions: Vec<[f32; 3]> = positions.collect();
            let indices: Vec<u32> = match reader.read_indices() {
                Some(i) => i.into_u32().collect(),
                None => (0..positions.len() as u32).collect(),
            };
            let normals: Vec<[f32; 3]> = match reader.read_normals() {
                Some(n) => n.collect(),
                None => smooth_normals(&positions, &indices),
            };
            let uvs: Option<Vec<[f32; 2]>> =
                reader.read_tex_coords(0).map(|t| t.into_f32().collect());
            missing_uvs |= uvs.is_none();

            let vertex = |i: u32| -> Result<Vertex> {
                let i = i as usize;
                let p = *positions.get(i).context("glTF index out of range")?;
                let pos = world.transform_point3(Vec3::from(p));
                let n = normal_mat * Vec3::from(normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0]));
                // glTF UVs have a top-left origin; SMD expects bottom-left.
                let uv = uvs.as_ref().and_then(|u| u.get(i)).copied().unwrap_or([0.0, 0.0]);
                Ok(Vertex {
                    pos: y_up_to_source(pos),
                    normal: y_up_to_source(n.try_normalize().unwrap_or(Vec3::Y)),
                    uv: [uv[0], 1.0 - uv[1]],
                })
            };
            for tri in indices.as_chunks::<3>().0 {
                let [a, b, c] = [vertex(tri[0])?, vertex(tri[1])?, vertex(tri[2])?];
                mesh.push(if mirrored { [a, c, b] } else { [a, b, c] }, material, part);
            }
        }
    }

    if missing_uvs {
        log("warning: some meshes have no UV map; they will show a single texel of the texture");
    }
    Ok(mesh)
}

fn load_obj(path: &Path, log: Logger) -> Result<Mesh> {
    // tobj's own triangulation is a plain fan, which covers openings in concave
    // n-gons, so faces are loaded as-is and ear-clipped below.
    let opts = tobj::LoadOptions {
        triangulate: false,
        single_index: true,
        ..Default::default()
    };
    // Only material *names* are used from the .mtl; textures come from the user.
    // A missing .mtl is fine: slots are then named by index.
    let (models, mtl) =
        tobj::load_obj(path, &opts).with_context(|| format!("failed to read OBJ {}", path.display()))?;
    let mtl = mtl.unwrap_or_default();

    let mut mesh = Mesh::default();
    let mut missing_uvs = false;
    for model in models {
        let m = &model.mesh;
        let material = match m.material_id {
            Some(id) => match mtl.get(id) {
                Some(mat) => mesh.material_slot(&mat.name),
                None => mesh.material_slot(&format!("material_{id}")),
            },
            None => mesh.material_slot("default"),
        };
        // tobj splits an object at every material change, keeping its name.
        let part = mesh.part_slot(&model.name);
        let positions: Vec<[f32; 3]> = m.positions.as_chunks::<3>().0.iter().map(|p| [p[0], p[1], p[2]]).collect();
        if m.indices.iter().any(|&i| i as usize >= positions.len()) {
            bail!("OBJ index out of range in object '{}'", model.name);
        }
        let indices = triangulate_faces(m, &positions);
        let normals: Vec<[f32; 3]> = if m.normals.len() == m.positions.len() {
            m.normals.as_chunks::<3>().0.iter().map(|n| [n[0], n[1], n[2]]).collect()
        } else {
            smooth_normals(&positions, &indices)
        };
        let has_uvs = m.texcoords.len() / 2 == positions.len();
        missing_uvs |= !has_uvs;

        let vertex = |i: u32| -> Vertex {
            let i = i as usize;
            let n = Vec3::from(normals[i]).try_normalize().unwrap_or(Vec3::Y);
            Vertex {
                pos: y_up_to_source(Vec3::from(positions[i])),
                normal: y_up_to_source(n),
                // OBJ UVs already have a bottom-left origin.
                uv: if has_uvs { [m.texcoords[i * 2], m.texcoords[i * 2 + 1]] } else { [0.0, 0.0] },
            }
        };
        for tri in indices.as_chunks::<3>().0 {
            mesh.push([vertex(tri[0]), vertex(tri[1]), vertex(tri[2])], material, part);
        }
    }
    if missing_uvs {
        log("warning: some objects have no UV map; they will show a single texel of the texture");
    }
    Ok(mesh)
}

/// Triangle indices for an OBJ mesh loaded with `triangulate: false`.
/// Faces with fewer than three corners (points, lines) are dropped.
fn triangulate_faces(m: &tobj::Mesh, positions: &[[f32; 3]]) -> Vec<u32> {
    if m.face_arities.is_empty() {
        return m.indices.clone(); // all triangles already
    }
    let mut out = Vec::with_capacity(m.indices.len() * 2);
    let mut start = 0;
    for &arity in &m.face_arities {
        let face = &m.indices[start..start + arity as usize];
        start += arity as usize;
        if face.len() == 3 {
            out.extend_from_slice(face);
            continue;
        }
        let corners: Vec<[f32; 3]> = face.iter().map(|&i| positions[i as usize]).collect();
        for t in triangulate_polygon(&corners) {
            out.extend(t.map(|k| face[k]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obj_axis_and_uv_conversion() {
        let dir = std::env::temp_dir().join(format!("bmod-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tri.obj");
        std::fs::write(
            &path,
            "v 0 0 0\nv 1 0 0\nv 0 1 -1\nvt 0 0\nvt 1 0\nvt 0 1\nvn 0 0 1\nf 1/1/1 2/2/1 3/3/1\n",
        )
        .unwrap();
        let mesh = load_model(&path, &|_| {}).unwrap();
        assert_eq!(mesh.triangles.len(), 1);
        let v = mesh.triangles[0][2];
        // OBJ (0, 1, -1) Y-up -> Source (0, 1, 1) Z-up.
        assert_eq!(v.pos, [0.0, 1.0, 1.0]);
        assert_eq!(v.uv, [0.0, 1.0]);
        // OBJ normal +Z (towards viewer) -> Source -Y.
        assert_eq!(mesh.triangles[0][0].normal, [0.0, -1.0, 0.0]);
        assert_eq!(mesh.materials, ["default"]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn obj_concave_ngon_is_ear_clipped() {
        let dir = std::env::temp_dir().join(format!("bmod-test-ngon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("u.obj");
        // A U-shaped 8-gon in the XY plane; a fan from corner 1 would cover the notch.
        std::fs::write(
            &path,
            "v 0 0 0\nv 4 0 0\nv 4 4 0\nv 3 4 0\nv 3 1 0\nv 1 1 0\nv 1 4 0\nv 0 4 0\nf 1 2 3 4 5 6 7 8\nf 1 2 3\n",
        )
        .unwrap();
        let mesh = load_model(&path, &|_| {}).unwrap();
        assert_eq!(mesh.triangles.len(), 6 + 1);
        // OBJ (x, y, 0) -> Source (x, 0, y): the notch centre is (2, _, 3).
        for t in &mesh.triangles[..6] {
            let [a, b, c] = t.map(|v| glam::Vec2::new(v.pos[0], v.pos[2]));
            let p = glam::Vec2::new(2.0, 3.0);
            let s = [(b - a).perp_dot(p - a), (c - b).perp_dot(p - b), (a - c).perp_dot(p - c)];
            assert!(!(s.iter().all(|&x| x > 0.0) || s.iter().all(|&x| x < 0.0)), "notch covered by {t:?}");
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn gltf_objects_become_parts_in_file_order() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/printer.glb");
        let mesh = load_model(&path, &|_| {}).unwrap();
        assert_eq!(mesh.parts, ["printer", "screen"]);
        assert_eq!(mesh.materials, ["body", "money"]);
        assert_eq!(mesh.part_triangle_counts(), [12, 2]);
    }

    #[test]
    fn obj_material_slots() {
        let dir = std::env::temp_dir().join(format!("bmod-test-mtl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("m.mtl"), "newmtl Body\nKd 1 1 1\nnewmtl Money.001\nKd 0 1 0\nnewmtl Unused\n").unwrap();
        let path = dir.join("m.obj");
        std::fs::write(
            &path,
            "mtllib m.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nv 1 1 0\n\
             o a\nusemtl Body\nf 1 2 3\nf 2 4 3\n\
             o b\nusemtl Money.001\nf 1 3 4\n",
        )
        .unwrap();
        let mesh = load_model(&path, &|_| {}).unwrap();
        assert_eq!(mesh.materials, ["body", "money_001"]);
        assert_eq!(mesh.material_triangle_counts(), [2, 1]);
        assert_eq!(mesh.parts, ["a", "b"]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn obj_object_with_several_materials_is_one_part() {
        let dir = std::env::temp_dir().join(format!("bmod-test-parts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("p.mtl"), "newmtl Wood\nnewmtl Metal\n").unwrap();
        let path = dir.join("p.obj");
        std::fs::write(
            &path,
            "mtllib p.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nv 1 1 0\n\
             o Crate\nusemtl Wood\nf 1 2 3\nusemtl Metal\nf 2 4 3\n\
             o Lid.001\nusemtl Wood\nf 1 3 4\n",
        )
        .unwrap();
        let mesh = load_model(&path, &|_| {}).unwrap();
        assert_eq!(mesh.parts, ["crate", "lid_001"]);
        assert_eq!(mesh.part_triangle_counts(), [2, 1]);
        assert_eq!(mesh.subset(&[1]).triangles.len(), 1);
        assert_eq!(mesh.subset(&[1]).materials, ["wood", "metal"]);
        std::fs::remove_dir_all(dir).ok();
    }
}
