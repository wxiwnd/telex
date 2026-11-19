//! CLI side of the Unix socket protocol, including on-demand daemon start.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde_json::Value;

use crate::paths::Paths;
use crate::protocol::{ApiError, Request, Response};
use crate::store::Store;

pub struct Client {
    paths: Paths,
}

impl Client {
    pub fn new(paths: Paths) -> Self {
        Self { paths }
    }

    fn socket(&self) -> PathBuf {
        self.paths.socket_path()
    }

    pub fn ensure_daemon(&self) -> Result<()> {
        if UnixStream::connect(self.socket()).is_ok() {
            return Ok(());
        }
        let store = Store::new(self.paths.clone());
        if !store.config_path().exists() {
            bail!("not initialized: run `tlx init` first");
        }
        store.load_config().context("read config.json")?;
        let run_dir = self.paths.runtime_dir();
        std::fs::create_dir_all(run_dir)?;
        let lock_path = self.paths.lock_path();
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;
        lock.lock_exclusive().context("lock maild start")?;

        if UnixStream::connect(self.socket()).is_ok() {
            return Ok(());
        }

        let exe = std::env::current_exe().context("locate tlx binary")?;
        Command::new(exe)
            .args(["maild", "--foreground"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("spawn maild")?;

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if UnixStream::connect(self.socket()).is_ok() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        bail!("maild did not become ready within 2s");
    }

    pub fn call(
        &self,
        method: &str,
        token: Option<&str>,
        params: Value,
    ) -> Result<Value, ApiError> {
        if let Err(err) = self.ensure_daemon() {
            return Err(ApiError::new("maild_unavailable", err.to_string()));
        }
        let mut stream = UnixStream::connect(self.socket())
            .map_err(|err| ApiError::new("maild_unavailable", err.to_string()))?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let request = Request {
            id: 1,
            method: method.to_string(),
            token: token.map(|value| value.to_string()),
            params,
        };
        let mut line = serde_json::to_string(&request)
            .map_err(|err| ApiError::new("protocol", err.to_string()))?;
        line.push('\n');
        stream
            .write_all(line.as_bytes())
            .map_err(|err| ApiError::new("maild_unavailable", err.to_string()))?;
        let mut reader = BufReader::new(stream);
        let mut response_line = String::new();
        reader
            .read_line(&mut response_line)
            .map_err(|err| ApiError::new("timeout", err.to_string()))?;
        let response: Response = serde_json::from_str(response_line.trim())
            .map_err(|err| ApiError::new("protocol", err.to_string()))?;
        if response.ok {
            Ok(response.result.unwrap_or(Value::Null))
        } else {
            Err(response
                .error
                .unwrap_or_else(|| ApiError::new("unknown", "unknown error")))
        }
    }
}
