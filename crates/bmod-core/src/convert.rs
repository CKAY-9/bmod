//! End-to-end conversion: model + texture -> self-contained GMod addon folder.
//!
//! Output layout (`<output_root>/bmod_<name>_<timestamp>/`):
//! ```text
//! models/bmod/<name>.mdl (+ .vvd, .phy, .dx80.vtx, .dx90.vtx)
//! materials/models/bmod/<name>/<name>.vtf
//! materials/models/bmod/<name>/<name>.vmt
//! src/<name>.qc, <name>_ref.smd, <name>_phys.smd
//! ```
//! The folder can be dropped straight into `garrysmod/addons/`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::compiler::Studiomdl;
use crate::{Logger, collision, import, qc, sanitize_name, smd, vmt, vtf};

/// Triangle count above which we warn that the prop is heavy for GMod.
const HIGH_POLY_WARNING: usize = 10_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlphaMode {
    #[default]
    Opaque,
    /// Hard-edged cut-outs (`$alphatest`): foliage, fences, decals.
    Cutout,
    /// Smooth blending (`$translucent`): glass, smoke.
    Translucent,
}

#[derive(Clone, Debug)]
pub struct ConvertOptions {
    pub model: PathBuf,
    pub texture: PathBuf,
    /// Asset name; sanitized. Empty means "derive from the model file name".
    pub name: String,
    /// Multiplier from model units to Source units (Blender metres -> 39.37).
    pub scale: f32,
    pub surfaceprop: String,
    pub alpha: AlphaMode,
    pub double_sided: bool,
    /// Directory in which the timestamped output folder is created.
    pub output_root: PathBuf,
}

#[derive(Clone, Debug)]
pub struct ConvertOutput {
    pub dir: PathBuf,
    pub mdl: PathBuf,
    pub model_path: String,
}

pub const DEFAULT_SCALE: f32 = 39.37;

pub fn convert(opts: &ConvertOptions, log: Logger) -> Result<ConvertOutput> {
    let name = match sanitize_name(&opts.name) {
        n if !n.is_empty() => n,
        _ => sanitize_name(&opts.model.file_stem().unwrap_or_default().to_string_lossy()),
    };
    if name.is_empty() {
        bail!("please give the model a name using letters or numbers");
    }
    if !(opts.scale.is_finite() && opts.scale > 0.0) {
        bail!("scale must be a positive number");
    }
    let surfaceprop = match sanitize_name(&opts.surfaceprop) {
        s if s.is_empty() => "default".to_string(),
        s => s,
    };

    // Fail fast on a missing toolchain before doing any work.
    let studiomdl = Studiomdl::locate()?;
    log(&format!("using studiomdl: {}", studiomdl.exe.display()));

    // --- Model ---
    log(&format!("loading model {}", opts.model.display()));
    let mut mesh = import::load_model(&opts.model, log)?;
    mesh.scale(opts.scale);
    let (min, max) = mesh.bounds();
    log(&format!(
        "{} triangles, size {:.1} x {:.1} x {:.1} units",
        mesh.triangles.len(),
        max[0] - min[0],
        max[1] - min[1],
        max[2] - min[2]
    ));
    if mesh.triangles.len() > HIGH_POLY_WARNING {
        log(&format!(
            "warning: {} triangles is a lot for a GMod prop; consider decimating below {HIGH_POLY_WARNING}",
            mesh.triangles.len()
        ));
    }

    // --- Texture ---
    log(&format!("loading texture {}", opts.texture.display()));
    let img = image::open(&opts.texture)
        .with_context(|| format!("failed to read texture {}", opts.texture.display()))?;
    let encoded = vtf::encode(&img);
    if let Some((w, h)) = encoded.resized_from {
        log(&format!(
            "warning: texture resized from {w}x{h} to {}x{} (Source needs power-of-two sizes)",
            encoded.width, encoded.height
        ));
    }
    log(&format!(
        "texture: {}x{} {:?}, {} mip levels",
        encoded.width, encoded.height, encoded.format, encoded.mip_count
    ));
    if encoded.format == vtf::VtfFormat::Dxt5 && opts.alpha == AlphaMode::Opaque {
        log("note: the texture has transparency; pick Cutout or Translucent to use it");
    }

    // --- Write files ---
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let dir = unique_dir(&opts.output_root.join(format!("bmod_{name}_{stamp}")));
    let src_dir = dir.join("src");
    let mat_rel = format!("models/bmod/{name}");
    let mat_dir = dir.join("materials").join(&mat_rel);
    fs::create_dir_all(&src_dir).with_context(|| format!("failed to create {}", src_dir.display()))?;
    fs::create_dir_all(&mat_dir)?;
    log(&format!("writing to {}", dir.display()));

    fs::write(mat_dir.join(format!("{name}.vtf")), &encoded.bytes)?;
    fs::write(
        mat_dir.join(format!("{name}.vmt")),
        vmt::generate(&format!("{mat_rel}/{name}"), opts.alpha, opts.double_sided),
    )?;

    let ref_smd = format!("{name}_ref.smd");
    let phys_smd = format!("{name}_phys.smd");
    fs::write(src_dir.join(&ref_smd), smd::reference(&mesh, &name))?;
    let hull = collision::convex_hull(&mesh);
    log(&format!("collision: convex hull with {} faces", hull.len()));
    fs::write(src_dir.join(&phys_smd), smd::physics(&hull))?;

    let model_path = format!("bmod/{name}.mdl");
    let qc_path = src_dir.join(format!("{name}.qc"));
    fs::write(
        &qc_path,
        qc::generate(&qc::QcParams {
            model_path: &model_path,
            cdmaterials: &format!("{mat_rel}/"),
            reference_smd: &ref_smd,
            physics_smd: &phys_smd,
            surfaceprop: &surfaceprop,
        }),
    )?;

    // --- Compile ---
    // A stub gameinfo.txt makes the output folder itself the "game" directory,
    // so studiomdl writes models/ here instead of into the GMod install.
    let gameinfo = dir.join("gameinfo.txt");
    fs::write(&gameinfo, GAMEINFO)?;
    let result = studiomdl.compile(&dir, &qc_path, log);
    let _ = fs::remove_file(&gameinfo);
    result?;

    let mdl = dir.join("models").join(&model_path);
    if !mdl.is_file() {
        bail!("studiomdl finished but {} was not created; see the log above", mdl.display());
    }
    log(&format!("done: models/{model_path}"));
    Ok(ConvertOutput { dir, mdl, model_path: format!("models/{model_path}") })
}

const GAMEINFO: &str = "\"GameInfo\"\n{\n\tgame \"BMod\"\n\tFileSystem\n\t{\n\t\tSteamAppId 4000\n\t\tSearchPaths\n\t\t{\n\t\t\tGame |gameinfo_path|.\n\t\t}\n\t}\n}\n";

/// Appends `_2`, `_3`, ... if a conversion already ran in the same second.
fn unique_dir(base: &Path) -> PathBuf {
    if !base.exists() {
        return base.to_path_buf();
    }
    (2..)
        .map(|i| PathBuf::from(format!("{}_{i}", base.display())))
        .find(|p| !p.exists())
        .unwrap()
}
