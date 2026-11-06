//! VMT (material) generation.

use crate::convert::{AlphaMode, Scroll};

pub struct VmtParams<'a> {
    /// Path relative to `materials/`, without extension.
    pub basetexture: &'a str,
    pub alpha: AlphaMode,
    pub double_sided: bool,
    pub scroll: Option<Scroll>,
}

pub fn generate(p: &VmtParams) -> String {
    let mut out = format!("\"VertexLitGeneric\"\n{{\n\t\"$basetexture\" \"{}\"\n", p.basetexture);
    match p.alpha {
        AlphaMode::Opaque => {}
        AlphaMode::Cutout => out.push_str("\t\"$alphatest\" \"1\"\n"),
        AlphaMode::Translucent => out.push_str("\t\"$translucent\" \"1\"\n"),
    }
    if p.double_sided {
        out.push_str("\t\"$nocull\" \"1\"\n");
    }
    if let Some(s) = p.scroll.filter(|s| s.rate != 0.0) {
        // Animates $basetexturetransform: `rate` texture-widths per second along `angle`.
        out.push_str(&format!(
            "\n\t\"Proxies\"\n\t{{\n\t\t\"TextureScroll\"\n\t\t{{\n\
             \t\t\t\"texturescrollvar\" \"$basetexturetransform\"\n\
             \t\t\t\"texturescrollrate\" \"{}\"\n\
             \t\t\t\"texturescrollangle\" \"{}\"\n\
             \t\t}}\n\t}}\n",
            s.rate, s.angle
        ));
    }
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_vmt() {
        let vmt = generate(&VmtParams {
            basetexture: "models/bmod/a/a",
            alpha: AlphaMode::Cutout,
            double_sided: true,
            scroll: None,
        });
        assert_eq!(
            vmt,
            "\"VertexLitGeneric\"\n{\n\t\"$basetexture\" \"models/bmod/a/a\"\n\t\"$alphatest\" \"1\"\n\t\"$nocull\" \"1\"\n}\n"
        );
    }

    #[test]
    fn writes_scroll_proxy() {
        let vmt = generate(&VmtParams {
            basetexture: "models/bmod/printer/money",
            alpha: AlphaMode::Opaque,
            double_sided: false,
            scroll: Some(Scroll { rate: 0.25, angle: 90.0 }),
        });
        assert_eq!(
            vmt,
            "\"VertexLitGeneric\"\n{\n\t\"$basetexture\" \"models/bmod/printer/money\"\n\n\
             \t\"Proxies\"\n\t{\n\t\t\"TextureScroll\"\n\t\t{\n\
             \t\t\t\"texturescrollvar\" \"$basetexturetransform\"\n\
             \t\t\t\"texturescrollrate\" \"0.25\"\n\
             \t\t\t\"texturescrollangle\" \"90\"\n\
             \t\t}\n\t}\n}\n"
        );
    }

    #[test]
    fn zero_rate_scroll_is_omitted() {
        let vmt = generate(&VmtParams {
            basetexture: "x",
            alpha: AlphaMode::Opaque,
            double_sided: false,
            scroll: Some(Scroll { rate: 0.0, angle: 45.0 }),
        });
        assert!(!vmt.contains("Proxies"));
    }
}
