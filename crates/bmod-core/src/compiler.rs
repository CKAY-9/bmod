//! Locating and running Valve's `studiomdl.exe`.
//!
//! studiomdl ships only with the Windows build of Garry's Mod, so on Linux
//! and macOS it runs under Wine in a BMod-owned prefix.
//!
//! Overrides: `BMOD_STUDIOMDL` (path to studiomdl.exe), `BMOD_WINE` (wine binary).

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::Logger;

#[derive(Clone, Debug)]
pub enum Runner {
    Native,
    Wine { wine: PathBuf, prefix: PathBuf },
}

#[derive(Clone, Debug)]
pub struct Studiomdl {
    pub exe: PathBuf,
    pub runner: Runner,
}

impl Studiomdl {
    /// Finds studiomdl and, off Windows, Wine. Errors explain how to fix the setup.
    pub fn locate() -> Result<Self> {
        let exe = find_studiomdl().context(
            "could not find studiomdl.exe. Install Garry's Mod through Steam (on Linux, force the \
             Windows build with Properties > Compatibility > Proton), or set BMOD_STUDIOMDL to its path",
        )?;
        let runner = if cfg!(windows) {
            Runner::Native
        } else {
            let wine = std::env::var_os("BMOD_WINE")
                .map(PathBuf::from)
                .or_else(|| find_in_path("wine"))
                .context("Wine is required to run studiomdl on this platform but `wine` was not found in PATH (or set BMOD_WINE)")?;
            let prefix = directories::ProjectDirs::from("", "", "bmod")
                .map(|d| d.data_dir().join("wineprefix"))
                .context("could not determine a data directory for the Wine prefix")?;
            Runner::Wine { wine, prefix }
        };
        Ok(Self { exe, runner })
    }

    /// Compiles `qc` with `game_dir` as the mod directory (it must contain a gameinfo.txt).
    /// Output lands in `game_dir/models/...`.
    pub fn compile(&self, game_dir: &Path, qc: &Path, log: Logger) -> Result<()> {
        let mut cmd = match &self.runner {
            Runner::Native => Command::new(&self.exe),
            Runner::Wine { wine, prefix } => {
                std::fs::create_dir_all(prefix)
                    .with_context(|| format!("failed to create Wine prefix {}", prefix.display()))?;
                let mut c = Command::new(wine);
                c.arg(&self.exe)
                    .env("WINEPREFIX", prefix)
                    .env("WINEDEBUG", "-all")
                    // Skip the Mono/Gecko install prompts when the prefix is first created.
                    .env("WINEDLLOVERRIDES", "mscoree,mshtml=");
                c
            }
        };
        cmd.args(["-nop4", "-game"])
            .arg(self.host_path(game_dir))
            .arg(self.host_path(qc))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(bin) = self.exe.parent() {
            cmd.current_dir(bin);
        }

        if matches!(self.runner, Runner::Wine { .. }) {
            log("running studiomdl under Wine (the first run sets up a Wine prefix and can take a minute)");
        }
        let mut child = cmd.spawn().context("failed to start studiomdl")?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let mut errors = Vec::new();
        std::thread::scope(|s| {
            s.spawn(|| pump(stderr, log, &mut Vec::new()));
            pump(stdout, log, &mut errors);
        });
        let status = child.wait().context("failed to wait for studiomdl")?;

        if let Some(err) = errors.last() {
            bail!("studiomdl failed: {err}");
        }
        if !status.success() {
            bail!("studiomdl exited with {status}");
        }
        Ok(())
    }

    /// Path as studiomdl sees it: native, or via Wine's default `Z:` mapping of `/`.
    fn host_path(&self, p: &Path) -> String {
        let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
        match self.runner {
            Runner::Native => abs.display().to_string(),
            Runner::Wine { .. } => format!("Z:{}", abs.display().to_string().replace('/', "\\")),
        }
    }
}

/// Forwards useful output lines to `log`, collecting error lines.
fn pump(stream: impl Read, log: Logger, errors: &mut Vec<String>) {
    for line in BufReader::new(stream).lines().map_while(Result::ok) {
        let line = line.trim_end();
        if line.is_empty() || is_wine_noise(line) {
            continue;
        }
        if line.contains("ERROR") {
            errors.push(line.to_string());
        }
        log(line);
    }
}

fn is_wine_noise(line: &str) -> bool {
    ["MESA", "pci id for fd", "fixme:", "err:", "wine: created the configuration directory"]
        .iter()
        .any(|p| line.starts_with(p))
}

fn find_in_path(bin: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(bin))
        .find(|p| p.is_file())
}

fn find_studiomdl() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("BMOD_STUDIOMDL") {
        return Some(PathBuf::from(p)).filter(|p| p.is_file());
    }
    steam_libraries()
        .into_iter()
        .map(|lib| lib.join("steamapps/common/GarrysMod/bin/studiomdl.exe"))
        .find(|p| p.is_file())
}

/// Steam library folders from every known Steam root's `libraryfolders.vdf`.
pub fn steam_libraries() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) {
        roots.extend([
            home.join(".local/share/Steam"),
            home.join(".steam/steam"),
            home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
            home.join("Library/Application Support/Steam"),
        ]);
    }
    roots.extend([
        PathBuf::from(r"C:\Program Files (x86)\Steam"),
        PathBuf::from(r"C:\Program Files\Steam"),
    ]);

    let mut libs: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        let canon = p.canonicalize().unwrap_or(p);
        if !libs.contains(&canon) {
            libs.push(canon);
        }
    };
    for root in roots.into_iter().filter(|r| r.is_dir()) {
        if let Ok(vdf) = std::fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) {
            parse_library_paths(&vdf).into_iter().for_each(&mut push);
        }
        push(root);
    }
    libs
}

/// Pulls the `"path" "..."` values out of a libraryfolders.vdf file.
fn parse_library_paths(vdf: &str) -> Vec<PathBuf> {
    vdf.lines()
        .filter_map(|line| {
            let mut parts = line.split('"').skip(1).step_by(2);
            (parts.next()? == "path").then(|| parts.next()).flatten()
        })
        .map(|p| PathBuf::from(p.replace(r"\\", r"\")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vdf_paths() {
        let vdf = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"/home/u/.local/share/Steam\"\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\SteamLibrary\"\n\t\t\"label\"\t\t\"\"\n\t}\n}\n";
        assert_eq!(
            parse_library_paths(vdf),
            vec![PathBuf::from("/home/u/.local/share/Steam"), PathBuf::from(r"D:\SteamLibrary")]
        );
    }

    #[test]
    fn wine_paths() {
        let s = Studiomdl {
            exe: "studiomdl.exe".into(),
            runner: Runner::Wine { wine: "wine".into(), prefix: "/tmp".into() },
        };
        assert_eq!(s.host_path(Path::new("/home/u/out/a.qc")), r"Z:\home\u\out\a.qc");
    }
}
