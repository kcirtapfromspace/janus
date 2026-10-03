//! The external programs ic runs, found the same way everywhere.
//!
//! Interview Coach.app carries its own ffmpeg and `ant` next to `ic` (Contents/MacOS), so a new Mac
//! needs nothing from Homebrew. Lookup order: an `IC_*` override, the copy bundled next to `ic`,
//! PATH, then the places each tool's installer uses. The app launches `ic` with a minimal PATH, so
//! those last locations are what make a Docker Desktop or OrbStack install visible.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Result, anyhow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Ffmpeg,
    Ant,
    Docker,
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::Ffmpeg => "ffmpeg",
            Tool::Ant => "ant",
            Tool::Docker => "docker",
        }
    }

    fn env_var(self) -> &'static str {
        match self {
            Tool::Ffmpeg => "IC_FFMPEG",
            Tool::Ant => "IC_ANT",
            Tool::Docker => "IC_DOCKER",
        }
    }

    fn bundled(self) -> bool {
        matches!(self, Tool::Ffmpeg | Tool::Ant)
    }

    /// Where installers put the tool when it isn't on the (minimal) PATH the app provides.
    fn install_dirs(self) -> Vec<PathBuf> {
        let home = dirs::home_dir().unwrap_or_default();
        let mut dirs = vec![PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")];
        if self == Tool::Docker {
            dirs.extend([
                home.join(".docker/bin"),
                PathBuf::from("/Applications/Docker.app/Contents/Resources/bin"),
                home.join(".orbstack/bin"),
                PathBuf::from("/Applications/OrbStack.app/Contents/MacOS/xbin"),
            ]);
        }
        dirs
    }

    pub fn find(self) -> Option<Found> {
        if let Some(path) = std::env::var_os(self.env_var()).map(PathBuf::from).filter(|p| p.is_file()) {
            return Some(Found { path, origin: Origin::Override });
        }
        if self.bundled()
            && let Some(path) = bundle_dir().map(|d| d.join(self.name())).filter(|p| p.is_file())
        {
            return Some(Found { path, origin: Origin::Bundled });
        }
        let on_path = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default();
        on_path
            .into_iter()
            .chain(self.install_dirs())
            .map(|dir| dir.join(self.name()))
            .find(|p| p.is_file())
            .map(|path| Found { path, origin: Origin::System })
    }

    /// The tool's path, or an error saying how to get it.
    pub fn require(self) -> Result<PathBuf> {
        self.find().map(|f| f.path).ok_or_else(|| {
            anyhow!(match self {
                Tool::Ffmpeg | Tool::Ant => format!(
                    "Interview Coach.app should include {0}, but it's missing — reinstall the app. (Running ic \
                     on its own? Set {1} to a {0} binary.)",
                    self.name(),
                    self.env_var()
                ),
                Tool::Docker => "Docker isn't installed. Get Docker Desktop from \
                                 https://www.docker.com/products/docker-desktop/ (or install OrbStack)."
                    .to_string(),
            })
        })
    }

    /// A command for this tool. Docker also gets its own folders on PATH, so the credential helper
    /// (`docker-credential-desktop`) and compose plugin resolve when ic runs from the app.
    pub fn command(self) -> Result<Command> {
        let path = self.require()?;
        let mut cmd = Command::new(&path);
        if self == Tool::Docker {
            let mut dirs: Vec<PathBuf> = path.parent().map(|p| vec![p.to_path_buf()]).unwrap_or_default();
            if let Ok(real) = path.canonicalize()
                && let Some(parent) = real.parent()
            {
                dirs.push(parent.to_path_buf());
            }
            dirs.extend(self.install_dirs());
            dirs.extend(std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default());
            if let Ok(joined) = std::env::join_paths(dirs) {
                cmd.env("PATH", joined);
            }
        }
        Ok(cmd)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Override,
    Bundled,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub origin: Origin,
}

/// The folder holding the running `ic`, resolved through symlinks (people link `ic` into
/// /opt/homebrew/bin), which inside the app is Contents/MacOS.
fn bundle_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.canonicalize().ok()?.parent().map(PathBuf::from)
}
