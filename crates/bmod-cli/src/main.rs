//! Headless front-end for the BMod pipeline (handy for testing and scripting).

use std::path::PathBuf;
use std::process::ExitCode;

use bmod_core::convert::{DEFAULT_CHUNKS, DEFAULT_SCALE, MAX_CHUNKS};
use bmod_core::{
    AlphaMode, Blank, Bodygroup, CollisionMode, ConvertOptions, MaterialOptions, Scroll, StudiomdlBuild, TextureQuality,
    ToolPaths, convert, inspect_model, texcache,
};

const USAGE: &str = "\
usage: bmod-cli <model> [texture] [options]

  texture                 default texture for every material slot
  --tex <slot>=<path>     texture for one slot (overrides the default)
  --scroll <slot>=<rate>[:<angle>]
                          scroll a slot's texture (rate in widths/sec, angle in degrees)
  --name <name>           asset name (default: model file name)
  --scale <n>             units per model unit (default: 39.37)
  --concave               concave collision that keeps holes open (cups, boxes)
  --group <name>=<part>[,<part>…][:shown|hidden|always]
                          bodygroup switching between the listed parts (objects in
                          the model file); with an empty option that is last
                          (shown, the default), first (hidden), or absent (always)
  --model-dir <dir>       folder under models/ (default: bmod)
  --out <dir>             where to create the output folder (default: current directory)
  --no-sources            delete the generated src/ (.qc, .smd) after compiling
  --fast-textures         much faster texture compression at somewhat lower quality
  --no-cache              always encode textures, without reusing earlier results
  --chunks <n>            split the model into up to n pieces (1-32, default 8) so
                          studiomdl compiles it much faster; 1 keeps it whole
  --no-fast-compile       also build the DirectX 8 mesh (.dx80.vtx), which GMod doesn't use
  --studiomdl-32bit       prefer GMod's 32-bit studiomdl over the faster 64-bit one
  --list                  print the model's material slots and parts, and exit";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn split_kv(v: &str) -> anyhow::Result<(&str, &str)> {
    v.split_once('=').ok_or_else(|| anyhow::anyhow!("expected <slot>=<value>, got '{v}'"))
}

fn run() -> anyhow::Result<()> {
    let mut positional = Vec::new();
    let mut textures: Vec<(String, PathBuf)> = Vec::new();
    let mut scrolls: Vec<(String, Scroll)> = Vec::new();
    let mut name = String::new();
    let mut scale = DEFAULT_SCALE;
    let mut list = false;
    let mut collision = CollisionMode::Convex;
    let mut model_dir = String::new();
    let mut output_root = std::env::current_dir()?;
    let mut keep_sources = true;
    let mut texture_quality = TextureQuality::High;
    let mut use_cache = true;
    let mut bodygroups = Vec::new();
    let mut chunks = DEFAULT_CHUNKS;
    let mut fast_compile = true;
    let mut build = StudiomdlBuild::X64;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| anyhow::anyhow!("{arg} needs a value\n\n{USAGE}"));
        match arg.as_str() {
            "--tex" => {
                let v = value()?;
                let (slot, path) = split_kv(&v)?;
                textures.push((slot.to_string(), path.into()));
            }
            "--scroll" => {
                let v = value()?;
                let (slot, spec) = split_kv(&v)?;
                let (rate, angle) = spec.split_once(':').unwrap_or((spec, "0"));
                scrolls.push((slot.to_string(), Scroll { rate: rate.parse()?, angle: angle.parse()? }));
            }
            "--group" => {
                let v = value()?;
                let (group, spec) = v.split_once('=').ok_or_else(|| anyhow::anyhow!("expected <name>=<parts>, got '{v}'"))?;
                let (parts, blank) = spec.rsplit_once(':').unwrap_or((spec, "shown"));
                let blank = match blank {
                    "shown" => Blank::Last,
                    "hidden" => Blank::First,
                    "always" => Blank::None,
                    other => anyhow::bail!("unknown --group mode '{other}' (use shown, hidden or always)"),
                };
                let parts = parts.split(',').map(str::trim).filter(|p| !p.is_empty()).map(String::from).collect();
                bodygroups.push(Bodygroup { name: group.to_string(), parts, blank });
            }
            "--name" => name = value()?,
            "--scale" => scale = value()?.parse()?,
            "--list" => list = true,
            "--concave" => collision = CollisionMode::Concave,
            "--model-dir" => model_dir = value()?,
            "--out" => output_root = value()?.into(),
            "--no-sources" => keep_sources = false,
            "--fast-textures" => texture_quality = TextureQuality::Fast,
            "--no-cache" => use_cache = false,
            "--chunks" => {
                chunks = value()?.parse()?;
                if !(1..=MAX_CHUNKS).contains(&chunks) {
                    anyhow::bail!("--chunks must be between 1 and {MAX_CHUNKS}");
                }
            }
            "--no-fast-compile" => fast_compile = false,
            "--studiomdl-32bit" => build = StudiomdlBuild::X86,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            _ => positional.push(arg),
        }
    }
    let [model, rest @ ..] = positional.as_slice() else { anyhow::bail!("{USAGE}") };
    let model = PathBuf::from(model);
    let default_texture = rest.first().map(PathBuf::from);

    let info = inspect_model(&model, &|line| println!("{line}"))?;
    if list {
        println!("materials:");
        for m in &info.materials {
            println!("  {}\t{} triangles", m.name, m.triangles);
        }
        println!("parts:");
        for p in &info.parts {
            println!("  {}\t{} triangles", p.name, p.triangles);
        }
        return Ok(());
    }

    let materials = info
        .materials
        .iter()
        .map(|slot| {
            let texture = textures
                .iter()
                .find(|(s, _)| *s == slot.name)
                .map(|(_, p)| p.clone())
                .or_else(|| default_texture.clone())
                .ok_or_else(|| anyhow::anyhow!("no texture for material '{}' (use --tex)", slot.name))?;
            Ok(MaterialOptions {
                slot: slot.name.clone(),
                texture,
                alpha: AlphaMode::Opaque,
                double_sided: false,
                scroll: scrolls.iter().find(|(s, _)| *s == slot.name).map(|(_, sc)| *sc),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let opts = ConvertOptions {
        model,
        materials,
        name,
        scale,
        surfaceprop: "default".into(),
        collision,
        bodygroups,
        model_dir,
        keep_sources,
        tools: ToolPaths { build, ..Default::default() },
        output_root,
        texture_quality,
        texture_cache: use_cache.then(texcache::default_dir).flatten(),
        fast_compile,
        chunks,
    };
    let out = convert(&opts, &|line| println!("{line}"))?;
    println!("output: {}", out.dir.display());
    Ok(())
}
