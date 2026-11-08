//! Stage timings for the conversion pipeline on synthetic heavy inputs.
//!
//! `cargo run --release -p bmod-core --example bench -- <asset_dir> [runs]`
//!
//! Generates (once) a ~100k-triangle concave OBJ "cup" and a few large
//! textures in `<asset_dir>`, then times each pipeline stage.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bmod_core::{
    AlphaMode, CollisionMode, ConvertOptions, MaterialOptions, TextureQuality, ToolPaths, collision, convert, import, smd, vtf,
};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("usage: bench <asset_dir> [runs]"));
    let runs: usize = args.next().map(|r| r.parse().unwrap()).unwrap_or(3);
    std::fs::create_dir_all(&dir)?;

    let obj = dir.join("cup.obj");
    if !obj.exists() {
        write_cup(&obj, 256, 200)?;
    }
    let textures = [("4096_rgb", 4096, 4096, false), ("4096_rgba", 4096, 4096, true), ("3000_rgb_npot", 3000, 3000, false)];
    for (name, w, h, alpha) in textures {
        let p = dir.join(format!("{name}.png"));
        if !p.exists() {
            write_texture(&p, w, h, alpha)?;
        }
    }

    let log = &|_: &str| {};
    println!("runs: {runs} (reporting min / median)\n");

    let mesh = time("load_model (cup.obj)", runs, || import::load_model(&obj, log).unwrap());
    println!("    {} triangles", mesh.triangles.len());
    let mut mesh = mesh;
    mesh.scale(39.37);

    time("smd::reference", runs, || smd::reference(&mesh));
    let all: Vec<u32> = (0..mesh.parts.len() as u32).collect();
    time("mesh.subset(all parts)", runs, || mesh.subset(&all));
    time("collision::convex_hull", runs, || collision::convex_hull(&mesh));
    time("collision::convex_decomposition", runs.min(2), || collision::convex_decomposition(&mesh));

    for (name, ..) in textures {
        let p = dir.join(format!("{name}.png"));
        let img = time(&format!("image::open {name}"), runs, || image::open(&p).unwrap());
        time(&format!("vtf::encode {name}"), runs, || vtf::encode(&img, TextureQuality::High));
        time(&format!("vtf::encode {name} [fast]"), runs, || vtf::encode(&img, TextureQuality::Fast));
    }

    // Whole convert() with a stub that stands in for studiomdl, so only BMod's own work is timed.
    // It fails at the end ("mdl was not created"), after everything else has run.
    println!();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let out = dir.join("out");
    let tex = |n: &str| dir.join(format!("{n}.png"));
    let mat = |slot: &str, texture: PathBuf| MaterialOptions {
        slot: slot.into(),
        texture,
        alpha: AlphaMode::Opaque,
        double_sided: false,
        scroll: None,
    };
    let cases = [
        ("convert cup + 4096 tex, convex", obj.clone(), vec![mat("default", tex("4096_rgb"))], CollisionMode::Convex),
        ("convert cup + 4096 tex, concave", obj.clone(), vec![mat("default", tex("4096_rgb"))], CollisionMode::Concave),
        (
            "convert printer + 2x4096 tex, concave",
            fixtures.join("printer.glb"),
            vec![mat("body", tex("4096_rgb")), mat("money", tex("4096_rgba"))],
            CollisionMode::Concave,
        ),
    ];
    let cache = dir.join("texcache");
    std::fs::remove_dir_all(&cache).ok();
    let variants = [("", TextureQuality::High, false), (" [fast]", TextureQuality::Fast, false), (" [cached]", TextureQuality::High, true)];
    for (label, model, materials, collision) in cases {
        let base = ConvertOptions {
            model,
            materials,
            name: String::new(),
            scale: 39.37,
            surfaceprop: String::new(),
            collision,
            bodygroups: Vec::new(),
            output_root: out.clone(),
            model_dir: String::new(),
            keep_sources: true,
            tools: ToolPaths { studiomdl: Some(fixtures.join("cube.png")), wine: Some("/usr/bin/true".into()) },
            texture_quality: TextureQuality::High,
            texture_cache: None,
        };
        let run = |opts: &ConvertOptions| {
            let err = convert(opts, log).unwrap_err().to_string();
            assert!(err.contains("was not created"), "{err}");
        };
        for (suffix, texture_quality, cached) in variants {
            let opts = ConvertOptions { texture_quality, texture_cache: cached.then(|| cache.clone()), ..base.clone() };
            if cached {
                run(&opts); // warm the cache
            }
            time(&format!("{label}{suffix}"), runs, || run(&opts));
        }
    }
    std::fs::remove_dir_all(&out).ok();
    std::fs::remove_dir_all(&cache).ok();
    Ok(())
}

fn time<T>(label: &str, runs: usize, mut f: impl FnMut() -> T) -> T {
    let mut times: Vec<Duration> = Vec::new();
    let mut out = None;
    for _ in 0..runs {
        let t = Instant::now();
        let r = std::hint::black_box(f());
        times.push(t.elapsed());
        out = Some(r);
    }
    times.sort();
    println!(
        "{label:<40} {:>9.1} ms  {:>9.1} ms",
        times[0].as_secs_f64() * 1e3,
        times[times.len() / 2].as_secs_f64() * 1e3
    );
    out.unwrap()
}

/// Lathed cup with walls and a base, open at the top: 2 * radial * profile triangles.
fn write_cup(path: &Path, radial: usize, profile: usize) -> anyhow::Result<()> {
    // Profile polyline (r, z) going outer-bottom-centre -> outer wall -> rim -> inner wall -> inner-bottom-centre.
    let mut prof: Vec<(f32, f32)> = Vec::new();
    let q = profile / 5;
    let lerp = |a: (f32, f32), b: (f32, f32), t: f32| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
    let pts = [(0.0, 0.0), (0.1, 0.0), (0.1, 0.2), (0.09, 0.2), (0.09, 0.01), (0.0, 0.01)];
    for seg in 0..5 {
        let n = if seg == 4 { profile - 4 * q } else { q };
        for k in 0..n {
            prof.push(lerp(pts[seg], pts[seg + 1], k as f32 / n as f32));
        }
    }
    prof.push(pts[5]);

    let mut s = String::new();
    for (j, &(r, z)) in prof.iter().enumerate() {
        for i in 0..=radial {
            let a = i as f32 / radial as f32 * std::f32::consts::TAU;
            // OBJ is Y-up.
            let _ = writeln!(s, "v {} {} {}", r * a.cos(), z, r * a.sin());
            let _ = writeln!(s, "vt {} {}", i as f32 / radial as f32, j as f32 / prof.len() as f32);
        }
    }
    let row = radial + 1;
    for j in 0..prof.len() - 1 {
        for i in 0..radial {
            let a = j * row + i + 1;
            let (b, c, d) = (a + 1, a + row + 1, a + row);
            let _ = writeln!(s, "f {a}/{a} {b}/{b} {c}/{c} {d}/{d}");
        }
    }
    std::fs::write(path, s)?;
    Ok(())
}

/// Noisy gradient so DXT compression can't take the single-colour fast path.
fn write_texture(path: &Path, w: u32, h: u32, alpha: bool) -> anyhow::Result<()> {
    let img = image::RgbaImage::from_fn(w, h, |x, y| {
        let mut n = x.wrapping_mul(374761393) ^ y.wrapping_mul(668265263);
        n = (n ^ (n >> 13)).wrapping_mul(1274126177);
        let noise = (n >> 24) as u8 / 4;
        let r = ((x * 255) / w) as u8;
        let g = ((y * 255) / h) as u8;
        let b = (((x + y) * 127) / (w + h)) as u8;
        let a = if alpha { (((x ^ y) & 0xff) as u8).max(16) } else { 255 };
        image::Rgba([r.saturating_add(noise), g.saturating_add(noise), b.saturating_add(noise), a])
    });
    img.save(path)?;
    Ok(())
}
