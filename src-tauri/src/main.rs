// Prevents an extra console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

use bmod_core::compiler::{Runner, Studiomdl};
use bmod_core::{AlphaMode, ConvertOptions, MaterialOptions, Scroll};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tauri_plugin_opener::OpenerExt;

const LOG_EVENT: &str = "bmod-log";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Environment {
    studiomdl: Option<String>,
    wine: Option<String>,
    problem: Option<String>,
    output_root: String,
    model_extensions: &'static [&'static str],
    texture_extensions: &'static [&'static str],
    default_scale: f32,
}

#[derive(Deserialize)]
struct ScrollRequest {
    rate: f32,
    angle: f32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MaterialRequest {
    slot: String,
    texture: PathBuf,
    alpha: String,
    double_sided: bool,
    scroll: Option<ScrollRequest>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConvertRequest {
    model: PathBuf,
    materials: Vec<MaterialRequest>,
    name: String,
    scale: f32,
    surfaceprop: String,
}

#[derive(Serialize)]
struct MaterialSlotInfo {
    name: String,
    triangles: usize,
}

#[derive(Serialize)]
struct ModelInfo {
    materials: Vec<MaterialSlotInfo>,
    triangles: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ConvertResult {
    dir: String,
    model_path: String,
}

/// The working directory, unless it is the filesystem root (as when launched
/// from a desktop menu), in which case the home directory is used instead.
fn output_root() -> PathBuf {
    std::env::current_dir()
        .ok()
        .filter(|p| p.parent().is_some())
        .or_else(|| std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

#[tauri::command]
fn environment() -> Environment {
    let (studiomdl, wine, problem) = match Studiomdl::locate() {
        Ok(s) => {
            let wine = match &s.runner {
                Runner::Wine { wine, .. } => Some(wine.display().to_string()),
                Runner::Native => None,
            };
            (Some(s.exe.display().to_string()), wine, None)
        }
        Err(e) => (None, None, Some(format!("{e:#}"))),
    };
    Environment {
        studiomdl,
        wine,
        problem,
        output_root: output_root().display().to_string(),
        model_extensions: bmod_core::MODEL_EXTENSIONS,
        texture_extensions: bmod_core::TEXTURE_EXTENSIONS,
        default_scale: bmod_core::convert::DEFAULT_SCALE,
    }
}

#[tauri::command]
async fn inspect_model(app: AppHandle, path: PathBuf) -> Result<ModelInfo, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let log = |line: &str| {
            let _ = app.emit(LOG_EVENT, line);
        };
        bmod_core::inspect_model(&path, &log)
            .map(|info| ModelInfo {
                triangles: info.triangles,
                materials: info
                    .materials
                    .into_iter()
                    .map(|m| MaterialSlotInfo { name: m.name, triangles: m.triangles })
                    .collect(),
            })
            .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| format!("model loading crashed: {e}"))?
}

#[tauri::command]
async fn convert(app: AppHandle, req: ConvertRequest) -> Result<ConvertResult, String> {
    let opts = ConvertOptions {
        model: req.model,
        materials: req
            .materials
            .into_iter()
            .map(|m| MaterialOptions {
                slot: m.slot,
                texture: m.texture,
                alpha: match m.alpha.as_str() {
                    "cutout" => AlphaMode::Cutout,
                    "translucent" => AlphaMode::Translucent,
                    _ => AlphaMode::Opaque,
                },
                double_sided: m.double_sided,
                scroll: m.scroll.map(|s| Scroll { rate: s.rate, angle: s.angle }),
            })
            .collect(),
        name: req.name,
        scale: req.scale,
        surfaceprop: req.surfaceprop,
        output_root: output_root(),
    };
    tauri::async_runtime::spawn_blocking(move || {
        let log = |line: &str| {
            let _ = app.emit(LOG_EVENT, line);
        };
        bmod_core::convert(&opts, &log)
            .map(|out| ConvertResult { dir: out.dir.display().to_string(), model_path: out.model_path })
            .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| format!("conversion task crashed: {e}"))?
}

#[tauri::command]
fn open_folder(app: AppHandle, path: String) -> Result<(), String> {
    app.opener().open_path(path, None::<&str>).map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![environment, inspect_model, convert, open_folder])
        .run(tauri::generate_context!())
        .expect("error while running BMod");
}
