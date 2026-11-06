# BMod

Blender-to-Garry's Mod, or BMod, is a simple GUI app for converting modern model files and an unwrapped texture into a Garry's Mod static prop: `.mdl` plus `.vtf`/`.vmt`.

## Requirements

- **Rust** (stable) and the [Tauri 2 system dependencies](https://v2.tauri.app/start/prerequisites/). On Linux these are `webkit2gtk-4.1` and `gtk3`.
- **Garry's Mod (Windows build)** installed through Steam. BMod uses its `bin/studiomdl.exe`. On Linux, force the Windows build with *Properties → Compatibility → Proton*.
- **Wine** on Linux and macOS, for running studiomdl. BMod uses its own prefix in the app data dir, for example `~/.local/share/bmod/wineprefix`.

BMod finds GMod through your Steam library folders. To override this, set:

| Variable         | Purpose                     |
|------------------|-----------------------------|
| `BMOD_STUDIOMDL` | Path to `studiomdl.exe`     |
| `BMOD_WINE`      | Path to the `wine` binary   |

## Run

```sh
cargo run -p bmod                                       # GUI
cargo run -p bmod-cli -- model.glb texture.png [name] [scale]   # headless
```

Bundling installers (AppImage/deb/MSI) needs the Tauri CLI: `cargo install tauri-cli`, then `cargo tauri build`.

## Usage

1. Drop or browse a model (`.glb`, `.gltf`, `.obj`) and its texture (`.png`, `.jpg`, `.tga`, …).
2. Set the name, scale, surface type, transparency and double-sided options as needed.
3. Click **Convert**. A folder named `bmod_<name>_<timestamp>/` appears in the working directory:

```text
models/bmod/<name>.mdl  (.vvd .phy .dx80.vtx .dx90.vtx)
materials/models/bmod/<name>/<name>.vtf
materials/models/bmod/<name>/<name>.vmt
src/            generated .qc and .smd files, kept for reference
```

Copy that folder into `garrysmod/addons/`. The prop then spawns as `models/bmod/<name>.mdl`.

### Preparing the model in Blender

- Use one object (join with Ctrl+J), one material and one UV map, with all transforms applied (Ctrl+A).
- Export as **glTF 2.0 (.glb)**. The default scale of 39.37 turns Blender metres into Source units, which are roughly inches.
- Use power-of-two textures, such as 1024×1024. Other sizes are resized automatically.

## v1 scope

- Static props only: no bones, animations or LODs.
- One material: the whole model uses the single texture you give it.
- Collision is a single convex hull. Concave shapes such as tables collide as their "shrink-wrap".
- The texture is DXT1, or DXT5 when it has alpha, with full mipmaps. There are no normal maps yet.

## Layout

```text
crates/bmod-core   conversion pipeline (import, SMD, QC, VTF/VMT, collision, studiomdl runner)
crates/bmod-cli    headless front-end
src-tauri          Tauri app shell
ui                 frontend (plain HTML/CSS/JS, no build step)
tests/fixtures     sample cube model and texture
```
