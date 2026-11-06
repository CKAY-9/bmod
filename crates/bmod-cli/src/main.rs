//! Headless front-end for the BMod pipeline (handy for testing and scripting).
//!
//! Usage: bmod-cli <model> <texture> [name] [scale]

use std::path::PathBuf;
use std::process::ExitCode;

use bmod_core::{AlphaMode, ConvertOptions, convert, convert::DEFAULT_SCALE};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [model, texture, rest @ ..] = args.as_slice() else {
        eprintln!("usage: bmod-cli <model.glb|gltf|obj> <texture.png|jpg|tga> [name] [scale]");
        return ExitCode::from(2);
    };
    let opts = ConvertOptions {
        model: PathBuf::from(model),
        texture: PathBuf::from(texture),
        name: rest.first().cloned().unwrap_or_default(),
        scale: rest.get(1).and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_SCALE),
        surfaceprop: "default".into(),
        alpha: AlphaMode::Opaque,
        double_sided: false,
        output_root: std::env::current_dir().expect("no working directory"),
    };
    match convert(&opts, &|line| println!("{line}")) {
        Ok(out) => {
            println!("output: {}", out.dir.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
