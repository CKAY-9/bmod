//! End-to-end conversion: model + textures -> self-contained GMod addon folder.
//!
//! Output layout (`<output_root>/bmod_<name>_<timestamp>/`, with `<dir>` the
//! model folder, `bmod` by default):
//! ```text
//! models/<dir>/<name>.mdl (+ .vvd, .phy, .dx80.vtx, .dx90.vtx)
//! materials/models/<dir>/<name>/<slot>.vmt   (one per material slot)
//! materials/models/<dir>/<name>/<slot>.vtf   (one per distinct texture file)
//! src/<name>.qc, <name>_ref.smd, <name>_phys.smd   (unless `keep_sources` is off)
//! src/<name>_part_<part>.smd                       (one per part in a bodygroup)
//! ```
//! The folder can be dropped straight into `garrysmod/addons/`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::compiler::{Studiomdl, ToolPaths};
use crate::{Logger, collision, import, qc, sanitize_name, smd, vmt, vtf};

/// Triangle count above which we warn that the prop is heavy for GMod.
const HIGH_POLY_WARNING: usize = 10_000;
/// studiomdl's limit on materials per model.
const MAX_MATERIALS: usize = 32;
/// studiomdl's limit on body parts (MAXSTUDIOBODYPARTS), one of which is the base `$body`.
const MAX_BODYGROUPS: usize = 31;
/// Name of the always-visible `$body`; bodygroups can't reuse it.
const BASE_BODY: &str = "body";

pub const DEFAULT_SCALE: f32 = 39.37;
/// Folder under `models/` and `materials/models/` used when none is given.
pub const DEFAULT_MODEL_DIR: &str = "bmod";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlphaMode {
    #[default]
    Opaque,
    /// Hard-edged cut-outs (`$alphatest`): foliage, fences, decals.
    Cutout,
    /// Smooth blending (`$translucent`): glass, smoke.
    Translucent,
}

/// How the physics model is built.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CollisionMode {
    /// One convex hull around everything. Cheap, but fills in holes and hollows.
    #[default]
    Convex,
    /// Several convex pieces (V-HACD) that keep openings such as a cup's mouth.
    Concave,
}

/// UV scrolling via the `TextureScroll` material proxy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scroll {
    /// Texture widths per second.
    pub rate: f32,
    /// Direction in degrees.
    pub angle: f32,
}

/// Whether a bodygroup has an empty option, and where.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Blank {
    /// No empty option: one of the parts is always shown.
    None,
    /// Empty option after the parts: shown by default, can be hidden.
    #[default]
    Last,
    /// Empty option before the parts: hidden by default, can be shown.
    First,
}

/// A `$bodygroup`: a set of parts of which exactly one (or none, with a blank
/// option) is shown at a time, switchable in-game. Parts not in any bodygroup
/// are always shown.
#[derive(Clone, Debug)]
pub struct Bodygroup {
    /// Shown in GMod's bodygroup menu; sanitized.
    pub name: String,
    /// Part names as reported by [`inspect_model`], in option order.
    pub parts: Vec<String>,
    pub blank: Blank,
}

/// Settings for one material slot of the model.
#[derive(Clone, Debug)]
pub struct MaterialOptions {
    /// Slot name as reported by [`inspect_model`].
    pub slot: String,
    pub texture: PathBuf,
    pub alpha: AlphaMode,
    pub double_sided: bool,
    pub scroll: Option<Scroll>,
}

#[derive(Clone, Debug)]
pub struct ConvertOptions {
    pub model: PathBuf,
    /// One entry per material slot in the model.
    pub materials: Vec<MaterialOptions>,
    /// Asset name; sanitized. Empty means "derive from the model file name".
    pub name: String,
    /// Multiplier from model units to Source units (Blender metres -> 39.37).
    pub scale: f32,
    pub surfaceprop: String,
    pub collision: CollisionMode,
    /// Toggleable parts. Empty means the whole model is one always-visible body.
    pub bodygroups: Vec<Bodygroup>,
    /// Directory in which the timestamped output folder is created.
    pub output_root: PathBuf,
    /// Folder under `models/` for the prop; sanitized. Empty means [`DEFAULT_MODEL_DIR`].
    pub model_dir: String,
    /// Keep the generated `src/` (QC and SMD files) after a successful compile.
    pub keep_sources: bool,
    pub tools: ToolPaths,
}

#[derive(Clone, Debug)]
pub struct ConvertOutput {
    pub dir: PathBuf,
    pub mdl: PathBuf,
    pub model_path: String,
}

#[derive(Clone, Debug)]
pub struct MaterialSlot {
    pub name: String,
    pub triangles: usize,
}

/// An object from the model file, which can be put in a bodygroup.
#[derive(Clone, Debug)]
pub struct ModelPart {
    pub name: String,
    pub triangles: usize,
}

#[derive(Clone, Debug)]
pub struct ModelInfo {
    pub materials: Vec<MaterialSlot>,
    pub parts: Vec<ModelPart>,
    pub triangles: usize,
}

/// Loads a model just to report its material slots, for building the UI.
pub fn inspect_model(path: &Path, log: Logger) -> Result<ModelInfo> {
    let mesh = import::load_model(path, log)?;
    let counts = mesh.material_triangle_counts();
    let part_counts = mesh.part_triangle_counts();
    Ok(ModelInfo {
        triangles: mesh.triangles.len(),
        materials: mesh
            .materials
            .into_iter()
            .zip(counts)
            .map(|(name, triangles)| MaterialSlot { name, triangles })
            .collect(),
        parts: mesh
            .parts
            .into_iter()
            .zip(part_counts)
            .map(|(name, triangles)| ModelPart { name, triangles })
            .collect(),
    })
}

/// Which parts go where, as indices into [`Mesh::parts`].
#[derive(Debug, PartialEq)]
struct BodyLayout {
    /// Always-visible parts.
    base: Vec<u32>,
    /// Bodygroup name and its options; `None` is the blank option.
    groups: Vec<(String, Vec<Option<u32>>)>,
}

impl BodyLayout {
    /// Parts visible when the prop spawns: the base plus option 0 of each group.
    fn default_visible(&self) -> Vec<u32> {
        let first = self.groups.iter().filter_map(|(_, options)| options[0]);
        self.base.iter().copied().chain(first).collect()
    }
}

/// Checks the requested bodygroups against the model's parts.
fn plan_bodies(parts: &[String], requested: &[Bodygroup], log: Logger) -> Result<BodyLayout> {
    let mut owner: Vec<Option<usize>> = vec![None; parts.len()];
    let mut groups: Vec<(String, Vec<Option<u32>>)> = Vec::new();
    for group in requested {
        let name = sanitize_name(&group.name);
        if name.is_empty() {
            bail!("bodygroup name '{}' needs letters or numbers", group.name);
        }
        if name == BASE_BODY {
            bail!("'{BASE_BODY}' is reserved for the always-visible parts; rename that bodygroup");
        }
        if groups.iter().any(|(n, _)| *n == name) {
            bail!("there are two bodygroups named '{name}'");
        }
        let mut options = Vec::new();
        for raw in &group.parts {
            let part = sanitize_name(raw);
            let Some(i) = parts.iter().position(|p| *p == part) else {
                log(&format!("warning: [{name}] ignoring unknown part '{raw}'"));
                continue;
            };
            match owner[i] {
                Some(g) if g == groups.len() => continue, // listed twice in this group
                Some(g) => bail!("part '{part}' is in two bodygroups: '{}' and '{name}'", groups[g].0),
                None => owner[i] = Some(groups.len()),
            }
            options.push(Some(i as u32));
        }
        if options.is_empty() {
            log(&format!("warning: skipping bodygroup '{name}', which has no parts"));
            continue;
        }
        match group.blank {
            Blank::None => {}
            Blank::Last => options.push(None),
            Blank::First => options.insert(0, None),
        }
        groups.push((name, options));
    }
    if groups.len() > MAX_BODYGROUPS {
        bail!("{} bodygroups is too many; Source allows at most {MAX_BODYGROUPS}", groups.len());
    }
    let base = (0..parts.len() as u32).filter(|&i| owner[i as usize].is_none()).collect();
    Ok(BodyLayout { base, groups })
}

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
    let model_dir = match sanitize_name(&opts.model_dir) {
        d if d.is_empty() => DEFAULT_MODEL_DIR.to_string(),
        d => d,
    };
    let surfaceprop = match sanitize_name(&opts.surfaceprop) {
        s if s.is_empty() => "default".to_string(),
        s => s,
    };
    for m in &opts.materials {
        if let Some(s) = m.scroll
            && !(s.rate.is_finite() && s.angle.is_finite())
        {
            bail!("scroll speed and angle for '{}' must be numbers", m.slot);
        }
    }

    // Fail fast on a missing toolchain before doing any work.
    let studiomdl = Studiomdl::locate_with(&opts.tools)?;
    log(&format!("using studiomdl: {}", studiomdl.exe.display()));

    // --- Model ---
    log(&format!("loading model {}", opts.model.display()));
    let mut mesh = import::load_model(&opts.model, log)?;
    mesh.scale(opts.scale);
    let (min, max) = mesh.bounds();
    log(&format!(
        "{} triangles, {} material(s), size {:.1} x {:.1} x {:.1} units",
        mesh.triangles.len(),
        mesh.materials.len(),
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
    if mesh.materials.len() > MAX_MATERIALS {
        bail!(
            "the model has {} materials; Source allows at most {MAX_MATERIALS}",
            mesh.materials.len()
        );
    }

    // Match every slot in the model to the caller's settings.
    let slot_opts: Vec<&MaterialOptions> = mesh
        .materials
        .iter()
        .map(|slot| opts.materials.iter().find(|m| m.slot == *slot))
        .collect::<Option<_>>()
        .with_context(|| {
            let missing: Vec<&str> = mesh
                .materials
                .iter()
                .filter(|s| !opts.materials.iter().any(|m| m.slot == **s))
                .map(String::as_str)
                .collect();
            format!("no texture chosen for material(s): {}", missing.join(", "))
        })?;
    for m in &opts.materials {
        if !mesh.materials.contains(&m.slot) {
            log(&format!("warning: ignoring settings for unknown material '{}'", m.slot));
        }
    }
    let bodies = plan_bodies(&mesh.parts, &opts.bodygroups, log)?;

    // --- Output folder ---
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let dir = unique_dir(&opts.output_root.join(format!("bmod_{name}_{stamp}")));
    let src_dir = dir.join("src");
    let mat_rel = format!("models/{model_dir}/{name}");
    let mat_dir = dir.join("materials").join(&mat_rel);
    fs::create_dir_all(&src_dir).with_context(|| format!("failed to create {}", src_dir.display()))?;
    fs::create_dir_all(&mat_dir)?;
    log(&format!("writing to {}", dir.display()));

    // --- Materials ---
    // Slots sharing a texture file share one VTF, named after the first slot using it.
    let mut vtf_for_texture: HashMap<PathBuf, String> = HashMap::new();
    for (slot, m) in mesh.materials.iter().zip(&slot_opts) {
        let key = fs::canonicalize(&m.texture).unwrap_or_else(|_| m.texture.clone());
        let vtf_name = match vtf_for_texture.get(&key) {
            Some(existing) => {
                log(&format!("[{slot}] reusing texture {existing}.vtf"));
                existing.clone()
            }
            None => {
                let has_alpha = write_vtf(&m.texture, &mat_dir.join(format!("{slot}.vtf")), slot, log)?;
                if has_alpha && m.alpha == AlphaMode::Opaque {
                    log(&format!(
                        "note: [{slot}] the texture has transparency; pick Cutout or Translucent to use it"
                    ));
                }
                vtf_for_texture.insert(key, slot.clone());
                slot.clone()
            }
        };
        if let Some(s) = m.scroll.filter(|s| s.rate != 0.0) {
            log(&format!("[{slot}] scrolling at {} per second, angle {}°", s.rate, s.angle));
        }
        fs::write(
            mat_dir.join(format!("{slot}.vmt")),
            vmt::generate(&vmt::VmtParams {
                basetexture: &format!("{mat_rel}/{vtf_name}"),
                alpha: m.alpha,
                double_sided: m.double_sided,
                scroll: m.scroll,
            }),
        )?;
    }

    // --- Geometry ---
    let ref_smd = format!("{name}_ref.smd");
    let phys_smd = format!("{name}_phys.smd");
    let reference_smd = if bodies.groups.is_empty() {
        fs::write(src_dir.join(&ref_smd), smd::reference(&mesh))?;
        Some(ref_smd.as_str())
    } else if !bodies.base.is_empty() {
        fs::write(src_dir.join(&ref_smd), smd::reference(&mesh.subset(&bodies.base)))?;
        Some(ref_smd.as_str())
    } else {
        None
    };
    let part_smd = |p: u32| format!("{name}_part_{}.smd", mesh.parts[p as usize]);
    for (group, options) in &bodies.groups {
        let labels: Vec<&str> = options
            .iter()
            .map(|o| o.map_or("(empty)", |p| mesh.parts[p as usize].as_str()))
            .collect();
        log(&format!("bodygroup {group}: {}", labels.join(", ")));
        for &p in options.iter().flatten() {
            fs::write(src_dir.join(part_smd(p)), smd::reference(&mesh.subset(&[p])))?;
        }
    }
    let part_smds: Vec<Vec<Option<String>>> =
        bodies.groups.iter().map(|(_, options)| options.iter().map(|o| o.map(part_smd)).collect()).collect();
    let qc_bodygroups: Vec<qc::QcBodygroup> = bodies
        .groups
        .iter()
        .zip(&part_smds)
        .map(|((group, _), smds)| qc::QcBodygroup { name: group, options: smds.iter().map(Option::as_deref).collect() })
        .collect();

    // Collision can't change with bodygroups; it follows what the prop spawns with.
    let solid = match bodies.default_visible() {
        _ if bodies.groups.is_empty() => None,
        visible if visible.is_empty() => {
            log("note: every part starts hidden, so the collision model covers all of them");
            None
        }
        visible => Some(mesh.subset(&visible)),
    };
    let solid = solid.as_ref().unwrap_or(&mesh);
    let concave_pieces = match opts.collision {
        CollisionMode::Convex => {
            let hull = collision::convex_hull(solid);
            log(&format!("collision: convex hull with {} faces", hull.len()));
            fs::write(src_dir.join(&phys_smd), smd::physics(&[hull]))?;
            None
        }
        CollisionMode::Concave => {
            log("collision: decomposing into convex pieces…");
            let pieces = collision::convex_decomposition(solid);
            let faces: usize = pieces.iter().map(Vec::len).sum();
            log(&format!("collision: {} convex piece(s), {faces} faces", pieces.len()));
            fs::write(src_dir.join(&phys_smd), smd::physics(&pieces))?;
            Some(pieces.len())
        }
    };

    let model_path = format!("{model_dir}/{name}.mdl");
    let qc_path = src_dir.join(format!("{name}.qc"));
    fs::write(
        &qc_path,
        qc::generate(&qc::QcParams {
            model_path: &model_path,
            cdmaterials: &format!("{mat_rel}/"),
            reference_smd,
            bodygroups: &qc_bodygroups,
            physics_smd: &phys_smd,
            surfaceprop: &surfaceprop,
            concave_pieces,
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
    if !opts.keep_sources {
        fs::remove_dir_all(&src_dir).with_context(|| format!("failed to remove {}", src_dir.display()))?;
    }
    log(&format!("done: models/{model_path}"));
    Ok(ConvertOutput { dir, mdl, model_path: format!("models/{model_path}") })
}

/// Encodes `texture` to `out`. Returns whether the texture has transparency.
fn write_vtf(texture: &Path, out: &Path, slot: &str, log: Logger) -> Result<bool> {
    log(&format!("[{slot}] loading texture {}", texture.display()));
    let img = image::open(texture).with_context(|| format!("failed to read texture {}", texture.display()))?;
    let encoded = vtf::encode(&img);
    if let Some((w, h)) = encoded.resized_from {
        log(&format!(
            "warning: [{slot}] texture resized from {w}x{h} to {}x{} (Source needs power-of-two sizes)",
            encoded.width, encoded.height
        ));
    }
    log(&format!(
        "[{slot}] texture: {}x{} {:?}, {} mip levels",
        encoded.width, encoded.height, encoded.format, encoded.mip_count
    ));
    fs::write(out, &encoded.bytes).with_context(|| format!("failed to write {}", out.display()))?;
    Ok(encoded.format == vtf::VtfFormat::Dxt5)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parts() -> Vec<String> {
        ["body", "lid", "hat_a", "hat_b"].map(String::from).to_vec()
    }

    fn group(name: &str, parts: &[&str], blank: Blank) -> Bodygroup {
        Bodygroup { name: name.into(), parts: parts.iter().map(|p| p.to_string()).collect(), blank }
    }

    #[test]
    fn plans_bodygroups() {
        let req = [group("Lid", &["lid"], Blank::Last), group("hat", &["hat_a", "Hat A", "hat_b"], Blank::First)];
        let layout = plan_bodies(&parts(), &req, &|_| {}).unwrap();
        assert_eq!(
            layout,
            BodyLayout {
                base: vec![0],
                groups: vec![("lid".into(), vec![Some(1), None]), ("hat".into(), vec![None, Some(2), Some(3)])],
            }
        );
        assert_eq!(layout.default_visible(), [0, 1]);
    }

    #[test]
    fn no_bodygroups_keeps_everything_in_the_base() {
        let layout = plan_bodies(&parts(), &[], &|_| {}).unwrap();
        assert_eq!(layout.base, [0, 1, 2, 3]);
        assert!(layout.groups.is_empty());
    }

    #[test]
    fn skips_unknown_parts_and_empty_groups() {
        let logged = std::sync::Mutex::new(Vec::new());
        let log = |l: &str| logged.lock().unwrap().push(l.to_string());
        let req = [group("ghost", &["nope"], Blank::Last), group("lid", &["lid", "nope"], Blank::None)];
        let layout = plan_bodies(&parts(), &req, &log).unwrap();
        assert_eq!(layout.groups, [("lid".into(), vec![Some(1)])]);
        assert_eq!(logged.lock().unwrap().len(), 3);
    }

    #[test]
    fn rejects_bad_bodygroups() {
        let err = |req: &[Bodygroup]| plan_bodies(&parts(), req, &|_| {}).unwrap_err().to_string();
        assert!(err(&[group("a", &["lid"], Blank::Last), group("b", &["lid"], Blank::Last)]).contains("two bodygroups"));
        assert!(err(&[group("x", &["lid"], Blank::Last), group("X", &["hat_a"], Blank::Last)]).contains("named 'x'"));
        assert!(err(&[group("Body", &["lid"], Blank::Last)]).contains("reserved"));
        assert!(err(&[group("!!", &["lid"], Blank::Last)]).contains("letters"));
    }
}
