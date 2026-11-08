//! Locating and running Valve's `studiomdl.exe`.
//!
//! studiomdl ships only with the Windows build of Garry's Mod, so on Linux
//! and macOS it runs under Wine in a BMod-owned prefix.
//!
//! Overrides, in order of precedence: explicit [`ToolPaths`] (the app's
//! settings), then `BMOD_STUDIOMDL` (path to studiomdl.exe) and `BMOD_WINE`
//! (wine binary), then auto-detection.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::Logger;

#[derive(Clone, Debug)]
pub enum Runner {
    Native,
    Wine { wine: PathBuf, prefix: PathBuf },
}

/// User-chosen tool locations; `None` means "auto-detect".
#[derive(Clone, Debug, Default)]
pub struct ToolPaths {
    pub studiomdl: Option<PathBuf>,
    pub wine: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct Studiomdl {
    pub exe: PathBuf,
    pub runner: Runner,
}

impl Studiomdl {
    /// Finds studiomdl and, off Windows, Wine. Errors explain how to fix the setup.
    pub fn locate() -> Result<Self> {
        Self::locate_with(&ToolPaths::default())
    }

    /// Like [`locate`](Self::locate), but explicit `paths` take precedence.
    pub fn locate_with(paths: &ToolPaths) -> Result<Self> {
        let exe = match &paths.studiomdl {
            Some(p) if p.is_file() => p.clone(),
            Some(p) => bail!("studiomdl.exe not found at {} (check BMod's settings)", p.display()),
            None => find_studiomdl().context(
                "could not find studiomdl.exe. Install Garry's Mod through Steam (on Linux, force the \
                 Windows build with Properties > Compatibility > Proton), or set its path in BMod's settings",
            )?,
        };
        let runner = if cfg!(windows) {
            Runner::Native
        } else {
            let wine = match &paths.wine {
                Some(p) if p.is_file() => p.clone(),
                Some(p) => bail!("Wine not found at {} (check BMod's settings)", p.display()),
                None => std::env::var_os("BMOD_WINE")
                    .map(PathBuf::from)
                    .or_else(|| find_in_path("wine"))
                    .context("Wine is required to run studiomdl on this platform but `wine` was not found in PATH (set its path in BMod's settings)")?,
            };
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

        // Wine's background services (services.exe, winedevice, ...) inherit the
        // pipes and keep them open for seconds after studiomdl exits, or forever
        // with a persistent wineserver. So stop at process exit, not at EOF; the
        // reader threads are left to finish on their own.
        let (tx, rx) = mpsc::channel::<(bool, String)>();
        let readers: [(Box<dyn Read + Send>, bool); 2] =
            [(Box::new(child.stdout.take().unwrap()), true), (Box::new(child.stderr.take().unwrap()), false)];
        for (stream, is_stdout) in readers {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    if tx.send((is_stdout, line)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);

        let mut errors = Vec::new();
        let mut handle = |(is_stdout, line): (bool, String)| {
            handle_line(&line, log, if is_stdout { Some(&mut errors) } else { None })
        };
        let status = loop {
            match rx.recv_timeout(POLL) {
                Ok(msg) => handle(msg),
                Err(RecvTimeoutError::Disconnected) => break child.wait(),
                Err(RecvTimeoutError::Timeout) => {}
            }
            if let Some(status) = child.try_wait().transpose() {
                break status;
            }
        }
        .context("failed to wait for studiomdl")?;
        // Pick up whatever studiomdl wrote just before exiting.
        let deadline = Instant::now() + DRAIN_LIMIT;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left.min(DRAIN_IDLE)) {
                Ok(msg) => handle(msg),
                Err(_) => break,
            }
        }
        drop(handle);

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

/// How often to check whether studiomdl has exited while waiting for output.
const POLL: Duration = Duration::from_millis(50);
/// After exit, stop draining output once it has been quiet this long...
const DRAIN_IDLE: Duration = Duration::from_millis(200);
/// ...or after this long in total.
const DRAIN_LIMIT: Duration = Duration::from_secs(1);

/// Forwards a useful output line to `log`, collecting error lines into `errors` if given.
fn handle_line(line: &str, log: Logger, errors: Option<&mut Vec<String>>) {
    let line = line.trim_end();
    if line.is_empty() || is_wine_noise(line) {
        return;
    }
    if let Some(errors) = errors
        && line.contains("ERROR")
    {
        errors.push(line.to_string());
    }
    log(line);
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
