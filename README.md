<div align="center">
<img src="./src-tauri/icons/icon.png" width=250 height=250 />
</div>

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
cargo run -p bmod                                    # GUI
cargo run -p bmod-cli -- model.glb --list            # show material slots
cargo run -p bmod-cli -- model.glb texture.png       # one texture for every slot
cargo run -p bmod-cli -- printer.glb --tex body=body.png --tex money=money.png \
    --scroll money=0.5:90 --name money_printer       # per-slot textures + scrolling
```

Bundling installers (AppImage/deb/MSI) needs the Tauri CLI: `cargo install tauri-cli`, then `cargo tauri build`.

## Usage

1. Drop or browse a model (`.glb`, `.gltf`, `.obj`). BMod lists its material slots.
2. Give each slot a texture (`.png`, `.jpg`, `.tga`, …) by clicking it or dropping an image onto it. Images dropped elsewhere fill the first empty slot.
3. For each slot, set transparency, double-sided, and **Scroll** (speed in texture-widths per second, and angle) as needed.
4. Set the name, scale and surface type, then click **Convert**. A folder named `bmod_<name>_<timestamp>/` appears in the working directory:

```text
models/bmod/<name>.mdl  (.vvd .phy .dx80.vtx .dx90.vtx)
materials/models/bmod/<name>/<slot>.vmt   one per material slot
materials/models/bmod/<name>/<slot>.vtf   one per distinct texture file
src/            generated .qc and .smd files, kept for reference
```

### Scrolling textures

Scrolling applies to a whole material, so give the moving part its own material in Blender, such as the money strip on a money printer. Make its texture tile seamlessly in the scroll direction. BMod writes a `TextureScroll` proxy on `$basetexturetransform`. If it scrolls the wrong way in-game, flip the angle (for example, 90 to -90).

Copy that folder into `garrysmod/addons/`. The prop then spawns as `models/bmod/<name>.mdl`.

### Preparing the model in Blender

- Use one object (join with Ctrl+J) and one UV map, with all transforms applied (Ctrl+A). Use one material per texture, up to 32. Material names become the slot names.
- Export as **glTF 2.0 (.glb)**. The default scale of 39.37 turns Blender metres into Source units, which are roughly inches.
- Use power-of-two textures, such as 1024×1024. Other sizes are resized automatically.

## v1 scope

- Static props only: no bones, animations or LODs.
- Textures can scroll but not flip through frames. There are no animated VTFs yet.
- Collision is a single convex hull. Concave shapes such as tables collide as their "shrink-wrap".
- The texture is DXT1, or DXT5 when it has alpha, with full mipmaps. There are no normal maps yet.

## Layout

```text
crates/bmod-core   conversion pipeline (import, SMD, QC, VTF/VMT, collision, studiomdl runner)
crates/bmod-cli    headless front-end
src-tauri          Tauri app shell
ui                 frontend (plain HTML/CSS/JS, no build step)
tests/fixtures     sample cube and money printer (regenerate with generate.py)
```
