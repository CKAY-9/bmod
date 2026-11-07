//! BMod conversion pipeline.
//!
//! Takes a modern model file (glTF/GLB/OBJ) plus an unwrapped texture and
//! produces a Garry's Mod static prop: `.mdl` (via studiomdl) and `.vtf`/`.vmt`.

pub mod collision;
pub mod compiler;
pub mod convert;
pub mod import;
pub mod mesh;
pub mod qc;
pub mod smd;
pub mod vmt;
pub mod vtf;

pub use compiler::ToolPaths;
pub use convert::{
    AlphaMode, CollisionMode, ConvertOptions, ConvertOutput, MaterialOptions, ModelInfo, Scroll, convert, inspect_model,
};

/// Receives human-readable progress lines during a conversion.
pub type Logger<'a> = &'a (dyn Fn(&str) + Sync);

/// File extensions accepted as model input.
pub const MODEL_EXTENSIONS: &[&str] = &["glb", "gltf", "obj"];

/// File extensions accepted as texture input.
pub const TEXTURE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "tga", "bmp", "webp"];

/// Turns arbitrary text into a safe Source asset name: lowercase `[a-z0-9_]`.
pub fn sanitize_name(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.trim().chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

#[cfg(test)]
mod tests {
    use super::sanitize_name;

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize_name("My Chair (v2).glb"), "my_chair_v2_glb");
        assert_eq!(sanitize_name("  __Crate--01 "), "crate_01");
        assert_eq!(sanitize_name("!!!"), "");
    }
}
