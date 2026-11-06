//! Model loading. Every loader returns a [`Mesh`] in Source space, still in
//! the file's own units (metres for Blender exports); scaling happens later.
//!
//! glTF and Blender's OBJ exporter are both Y-up with -Z forward, which maps
//! onto Source/Blender space as `(x, y, z) -> (x, -z, y)`.

use std::path::Path;

use anyhow::{Context, Result, bail};
use glam::{Mat3, Mat4, Vec3};

use crate::Logger;
use crate::mesh::{Mesh, Vertex, smooth_normals};

fn y_up_to_source(v: Vec3) -> [f32; 3] {
    [v.x, -v.z, v.y]
}

pub fn load_model(path: &Path, log: Logger) -> Result<Mesh> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let mesh = match ext.as_str() {
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
    let mut stack: Vec<(gltf::Node, Mat4)> =
        scene.nodes().map(|n| (n, Mat4::IDENTITY)).collect();

    while let Some((node, parent)) = stack.pop() {
        let world = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
        stack.extend(node.children().map(|c| (c, world)));

        let Some(gmesh) = node.mesh() else { continue };
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
                mesh.triangles.push(if mirrored { [a, c, b] } else { [a, b, c] });
            }
        }
    }

    if missing_uvs {
        log("warning: some meshes have no UV map; they will show a single texel of the texture");
    }
    Ok(mesh)
}

fn load_obj(path: &Path, log: Logger) -> Result<Mesh> {
    let opts = tobj::LoadOptions {
        triangulate: true,
        single_index: true,
        ..Default::default()
    };
    // Materials (.mtl) are irrelevant here: the texture comes from the user.
    let (models, _materials) =
        tobj::load_obj(path, &opts).with_context(|| format!("failed to read OBJ {}", path.display()))?;

    let mut mesh = Mesh::default();
    let mut missing_uvs = false;
    for model in models {
        let m = &model.mesh;
        let positions: Vec<[f32; 3]> = m.positions.as_chunks::<3>().0.iter().map(|p| [p[0], p[1], p[2]]).collect();
        let normals: Vec<[f32; 3]> = if m.normals.len() == m.positions.len() {
            m.normals.as_chunks::<3>().0.iter().map(|n| [n[0], n[1], n[2]]).collect()
        } else {
            smooth_normals(&positions, &m.indices)
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
        for tri in m.indices.as_chunks::<3>().0 {
            if tri.iter().any(|&i| i as usize >= positions.len()) {
                bail!("OBJ index out of range in object '{}'", model.name);
            }
            mesh.triangles.push([vertex(tri[0]), vertex(tri[1]), vertex(tri[2])]);
        }
    }
    if missing_uvs {
        log("warning: some objects have no UV map; they will show a single texel of the texture");
    }
    Ok(mesh)
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
        std::fs::remove_dir_all(dir).ok();
    }
}
