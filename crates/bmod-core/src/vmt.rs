//! VMT (material) generation.

use crate::convert::AlphaMode;

pub fn generate(basetexture: &str, alpha: AlphaMode, double_sided: bool) -> String {
    let mut out = format!("\"VertexLitGeneric\"\n{{\n\t\"$basetexture\" \"{basetexture}\"\n");
    match alpha {
        AlphaMode::Opaque => {}
        AlphaMode::Cutout => out.push_str("\t\"$alphatest\" \"1\"\n"),
        AlphaMode::Translucent => out.push_str("\t\"$translucent\" \"1\"\n"),
    }
    if double_sided {
        out.push_str("\t\"$nocull\" \"1\"\n");
    }
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_vmt() {
        assert_eq!(
            generate("models/bmod/a/a", AlphaMode::Cutout, true),
            "\"VertexLitGeneric\"\n{\n\t\"$basetexture\" \"models/bmod/a/a\"\n\t\"$alphatest\" \"1\"\n\t\"$nocull\" \"1\"\n}\n"
        );
    }
}
