//! XDG base directories used by Telex.
//!
//! Every path is derived per the XDG Base Directory specification. `HOME` is
//! required; a missing or relative XDG variable falls back to its default.

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

#[derive(Debug, Clone)]
pub struct Paths {
    config_dir: PathBuf,
    data_dir: PathBuf,
    state_dir: PathBuf,
    cache_dir: PathBuf,
    runtime_dir: PathBuf,
}

impl Paths {
    pub fn from_env() -> Result<Self> {
        let home = env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("HOME is not set"))?;
        let config_dir = xdg_dir("XDG_CONFIG_HOME", home.join(".config")).join("telex");
        let data_dir = xdg_dir("XDG_DATA_HOME", home.join(".local/share")).join("telex");
        let state_dir = xdg_dir("XDG_STATE_HOME", home.join(".local/state")).join("telex");
        let cache_dir = xdg_dir("XDG_CACHE_HOME", home.join(".cache")).join("telex");
        let runtime_dir = match xdg_value("XDG_RUNTIME_DIR") {
            Some(base) => base.join("telex"),
            // XDG_RUNTIME_DIR has no defined default. Keep the fallback inside
            // the state directory so every path stays under XDG.
            None => state_dir.join("run"),
        };
        Ok(Self {
            config_dir,
            data_dir,
            state_dir,
            cache_dir,
            runtime_dir,
        })
    }

    /// Single-root layout for tests and embedders.
    pub fn at_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            config_dir: root.join("config").join("telex"),
            data_dir: root.join("data").join("telex"),
            state_dir: root.join("state").join("telex"),
            cache_dir: root.join("cache").join("telex"),
            runtime_dir: root.join("run").join("telex"),
        }
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.json")
    }

    pub fn index_file(&self) -> PathBuf {
        self.cache_dir.join("index.sqlite3")
    }

    pub fn seats_dir(&self) -> PathBuf {
        self.data_dir.join("seats")
    }

    pub fn mail_dir(&self) -> PathBuf {
        self.data_dir.join("mail")
    }

    pub fn markers_dir(&self) -> PathBuf {
        self.data_dir.join("markers")
    }

    pub fn claims_dir(&self) -> PathBuf {
        self.state_dir.join("claims")
    }

    pub fn auth_dir(&self) -> PathBuf {
        self.state_dir.join("auth")
    }

    pub fn tokens_dir(&self) -> PathBuf {
        self.state_dir.join("tokens")
    }

    pub fn revoked_dir(&self) -> PathBuf {
        self.state_dir.join("revoked")
    }

    pub fn pending_dir(&self) -> PathBuf {
        self.state_dir.join("pending")
    }

    pub fn wake_dir(&self) -> PathBuf {
        self.state_dir.join("wake")
    }

    pub fn hook_dir(&self) -> PathBuf {
        self.state_dir.join("hook")
    }

    pub fn socket_path(&self) -> PathBuf {
        self.runtime_dir.join("maild.sock")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.runtime_dir.join("maild.lock")
    }
}

fn xdg_value(name: &str) -> Option<PathBuf> {
    let value = env::var_os(name)?;
    if value.is_empty() {
        return None;
    }
    let path = PathBuf::from(value);
    path.is_absolute().then_some(path)
}

fn xdg_dir(name: &str, default: PathBuf) -> PathBuf {
    xdg_value(name).unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_layout_is_isolated() {
        let paths = Paths::at_root("/tmp/telex-test");
        assert_eq!(
            paths.config_file(),
            Path::new("/tmp/telex-test/config/telex/config.json")
        );
        assert_eq!(
            paths.index_file(),
            Path::new("/tmp/telex-test/cache/telex/index.sqlite3")
        );
        assert_eq!(
            paths.mail_dir(),
            Path::new("/tmp/telex-test/data/telex/mail")
        );
        assert_eq!(
            paths.auth_dir(),
            Path::new("/tmp/telex-test/state/telex/auth")
        );
        assert_eq!(
            paths.socket_path(),
            Path::new("/tmp/telex-test/run/telex/maild.sock")
        );
    }
}
