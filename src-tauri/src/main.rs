// Prevents an extra console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use bmod_core::compiler::{Runner, Studiomdl};
use bmod_core::{AlphaMode, Blank, Bodygroup, CollisionMode, ConvertOptions, MaterialOptions, Scroll, TextureQuality, ToolPaths};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;

const LOG_EVENT: &str = "bmod-log";
const SETTINGS_FILE: &str = "settings.json";

/// Global, persisted app settings. Empty strings mean "use the default".
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Settings {
    output_dir: String,
    model_dir: String,
    keep_sources: bool,
    open_after_convert: bool,
    /// Range-fit DXT compression: much faster, somewhat lower quality.
    fast_textures: bool,
    default_scale: f32,
    default_collision: String,
    studiomdl_path: String,
    wine_path: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            output_dir: String::new(),
            model_dir: bmod_core::convert::DEFAULT_MODEL_DIR.into(),
            keep_sources: true,
            open_after_convert: false,
            fast_textures: false,
            default_scale: bmod_core::convert::DEFAULT_SCALE,
            default_collision: "convex".into(),
            studiomdl_path: String::new(),
            wine_path: String::new(),
        }
    }
}

impl Settings {
    fn output_root(&self) -> PathBuf {
        match self.output_dir.as_str() {
            "" => default_output_root(),
            dir => PathBuf::from(dir),
        }
    }

    fn tools(&self) -> ToolPaths {
        let path = |s: &str| (!s.is_empty()).then(|| PathBuf::from(s));
        ToolPaths { studiomdl: path(&self.studiomdl_path), wine: path(&self.wine_path) }
    }

    /// Trims and checks user input, returning a message for the first problem.
    fn normalized(mut self) -> Result<Self, String> {
        for s in [&mut self.output_dir, &mut self.studiomdl_path, &mut self.wine_path] {
            *s = s.trim().to_string();
        }
        self.model_dir = match bmod_core::sanitize_name(&self.model_dir) {
            d if d.is_empty() => bmod_core::convert::DEFAULT_MODEL_DIR.into(),
            d => d,
        };
        if !self.output_dir.is_empty() && !Path::new(&self.output_dir).is_absolute() {
            return Err("the export folder must be a full path".into());
        }
        if !(self.default_scale.is_finite() && self.default_scale > 0.0) {
            return Err("the default scale must be a positive number".into());
        }
        if !matches!(self.default_collision.as_str(), "convex" | "concave") {
            self.default_collision = "convex".into();
        }
        for (label, path) in [("studiomdl.exe", &self.studiomdl_path), ("Wine", &self.wine_path)] {
            if !path.is_empty() && !Path::new(path).is_file() {
                return Err(format!("{label} was not found at {path}"));
            }
        }
        Ok(self)
    }
}

struct AppState {
    settings: Mutex<Settings>,
}

impl AppState {
    fn settings(&self) -> Settings {
        self.settings.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

fn settings_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join(SETTINGS_FILE))
}

/// Saved settings, or defaults if there are none or the file is unreadable.
fn load_settings(app: &AppHandle) -> Settings {
    settings_path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Environment {
    studiomdl: Option<String>,
    wine: Option<String>,
    problem: Option<String>,
    output_root: String,
    default_output_root: String,
    is_windows: bool,
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
struct BodygroupRequest {
    name: String,
    parts: Vec<String>,
    /// "none", "last" (shown by default) or "first" (hidden by default).
    blank: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConvertRequest {
    model: PathBuf,
    materials: Vec<MaterialRequest>,
    name: String,
    scale: f32,
    surfaceprop: String,
    #[serde(default)]
    concave: bool,
    #[serde(default)]
    bodygroups: Vec<BodygroupRequest>,
}

#[derive(Serialize)]
struct MaterialSlotInfo {
    name: String,
    triangles: usize,
}

#[derive(Serialize)]
struct ModelInfo {
    materials: Vec<MaterialSlotInfo>,
    /// Same shape as a material slot: name and triangle count.
    parts: Vec<MaterialSlotInfo>,
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
fn default_output_root() -> PathBuf {
    std::env::current_dir()
        .ok()
        .filter(|p| p.parent().is_some())
        .or_else(|| std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

#[tauri::command]
fn environment(state: State<AppState>) -> Environment {
    let settings = state.settings();
    let (studiomdl, wine, problem) = match Studiomdl::locate_with(&settings.tools()) {
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
        output_root: settings.output_root().display().to_string(),
        default_output_root: default_output_root().display().to_string(),
        is_windows: cfg!(windows),
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
                parts: info
                    .parts
                    .into_iter()
                    .map(|p| MaterialSlotInfo { name: p.name, triangles: p.triangles })
                    .collect(),
            })
            .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| format!("model loading crashed: {e}"))?
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> Settings {
    state.settings()
}

/// Validates, persists and applies new settings; returns them as stored.
#[tauri::command]
fn save_settings(app: AppHandle, state: State<AppState>, settings: Settings) -> Result<Settings, String> {
    let settings = settings.normalized()?;
    let path = settings_path(&app).ok_or("could not determine a config directory")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    *state.settings.lock().unwrap_or_else(|e| e.into_inner()) = settings.clone();
    Ok(settings)
}

#[tauri::command]
async fn convert(app: AppHandle, state: State<'_, AppState>, req: ConvertRequest) -> Result<ConvertResult, String> {
    let settings = state.settings();
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
        collision: if req.concave { CollisionMode::Concave } else { CollisionMode::Convex },
        bodygroups: req
            .bodygroups
            .into_iter()
            .map(|g| Bodygroup {
                name: g.name,
                parts: g.parts,
                blank: match g.blank.as_str() {
                    "none" => Blank::None,
                    "first" => Blank::First,
                    _ => Blank::Last,
                },
            })
            .collect(),
        output_root: settings.output_root(),
        model_dir: settings.model_dir.clone(),
        keep_sources: settings.keep_sources,
        tools: settings.tools(),
        texture_quality: if settings.fast_textures { TextureQuality::Fast } else { TextureQuality::High },
        texture_cache: bmod_core::texcache::default_dir(),
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

/// Where encoded textures are cached and how much is in there.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TextureCacheInfo {
    /// `None` if there is no per-user cache directory, so caching is off.
    path: Option<String>,
    entries: usize,
    bytes: u64,
    limit_bytes: u64,
}

fn texture_cache_info_at(dir: Option<&Path>) -> Result<TextureCacheInfo, String> {
    let usage = match dir {
        Some(dir) => bmod_core::texcache::usage(dir).map_err(|e| format!("failed to read {}: {e}", dir.display()))?,
        None => Default::default(),
    };
    Ok(TextureCacheInfo {
        path: dir.map(|d| d.display().to_string()),
        entries: usage.entries,
        bytes: usage.bytes,
        limit_bytes: bmod_core::texcache::MAX_BYTES,
    })
}

#[tauri::command]
fn texture_cache_info() -> Result<TextureCacheInfo, String> {
    texture_cache_info_at(bmod_core::texcache::default_dir().as_deref())
}

/// Deletes every cached texture; returns the (now empty) cache info.
#[tauri::command]
fn clear_texture_cache(app: AppHandle) -> Result<TextureCacheInfo, String> {
    let dir = bmod_core::texcache::default_dir();
    if let Some(dir) = &dir {
        let removed =
            bmod_core::texcache::clear(dir).map_err(|e| format!("failed to clear {}: {e}", dir.display()))?;
        let _ = app.emit(
            LOG_EVENT,
            format!("cleared texture cache: {} texture(s), {:.1} MB", removed.entries, removed.bytes as f64 / 1e6),
        );
    }
    texture_cache_info_at(dir.as_deref())
}

/// Opens the texture cache folder, creating it if no texture has been cached yet.
#[tauri::command]
fn open_texture_cache(app: AppHandle) -> Result<(), String> {
    let dir = bmod_core::texcache::default_dir().ok_or("there is no texture cache on this system")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
    app.opener().open_path(dir.display().to_string(), None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
fn open_folder(app: AppHandle, path: String) -> Result<(), String> {
    app.opener().open_path(path, None::<&str>).map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let settings = load_settings(app.handle());
            app.manage(AppState { settings: Mutex::new(settings) });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            environment,
            inspect_model,
            convert,
            open_folder,
            get_settings,
            save_settings,
            texture_cache_info,
            clear_texture_cache,
            open_texture_cache
        ])
        .run(tauri::generate_context!())
        .expect("error while running BMod");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_settings() {
        let s = Settings {
            output_dir: "  ".into(),
            model_dir: " My Props! ".into(),
            default_collision: "bogus".into(),
            ..Default::default()
        }
        .normalized()
        .unwrap();
        assert_eq!(s.output_dir, "");
        assert_eq!(s.model_dir, "my_props");
        assert_eq!(s.default_collision, "convex");
        assert_eq!(Settings { model_dir: "!!".into(), ..Default::default() }.normalized().unwrap().model_dir, "bmod");
    }

    #[test]
    fn rejects_bad_settings() {
        let bad = |s: Settings| s.normalized().unwrap_err();
        assert!(bad(Settings { output_dir: "relative/dir".into(), ..Default::default() }).contains("full path"));
        assert!(bad(Settings { default_scale: 0.0, ..Default::default() }).contains("scale"));
        assert!(bad(Settings { studiomdl_path: "/no/such/studiomdl.exe".into(), ..Default::default() }).contains("not found"));
    }

    #[test]
    fn missing_fields_use_defaults() {
        let s: Settings = serde_json::from_str(r#"{"outputDir": "/x"}"#).unwrap();
        assert_eq!(s.output_dir, "/x");
        assert!(s.keep_sources);
        assert_eq!(s.model_dir, "bmod");
    }
}
