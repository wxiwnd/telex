use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

const PASSWORD: &str = "admin-password-123";

#[test]
fn login_finds_the_session_id_written_by_the_hook() {
    let dir = TempDir::new().expect("temp");
    let root = dir.path().join("root");
    std::fs::create_dir_all(root.join("run")).expect("run");
    let env = XdgEnv::new(&root);

    let setup = env.run(
        &root,
        &[
            "setup",
            "--password-stdin",
            "--role",
            "lead@local",
            "--json",
        ],
        Some(PASSWORD),
    );
    assert!(setup.status.success(), "setup: {}", stderr(&setup));

    // The hook knows the harness session id and records it for descendant
    // commands. No TELEX_SESSION_ID is provided.
    let hook = env.run(
        &root,
        &["hook", "--event", "UserPromptSubmit", "--harness", "test"],
        Some(r#"{"session_id":"handoff-session","hook_event_name":"UserPromptSubmit","cwd":"."}"#),
    );
    assert!(hook.status.success(), "hook: {}", stderr(&hook));

    let login = env.run(&root, &["login", "lead@local"], None);
    assert!(login.status.success(), "login: {}", stderr(&login));
    let code = stdout(&login)
        .lines()
        .find_map(|line| line.strip_prefix("pairing code: "))
        .expect("pairing code")
        .trim()
        .to_string();

    let approve = env.run(
        &root,
        &["approve", &code, "--password-stdin"],
        Some(PASSWORD),
    );
    assert!(approve.status.success(), "approve: {}", stderr(&approve));

    let whoami = env.run(&root, &["whoami"], None);
    assert!(whoami.status.success(), "whoami: {}", stderr(&whoami));
    assert!(stdout(&whoami).contains("lead@local"));
}

struct XdgEnv {
    config: PathBuf,
    data: PathBuf,
    state: PathBuf,
    cache: PathBuf,
    runtime: PathBuf,
}

impl XdgEnv {
    fn new(root: &Path) -> Self {
        Self {
            config: root.join("config"),
            data: root.join("data"),
            state: root.join("state"),
            cache: root.join("cache"),
            runtime: root.join("run"),
        }
    }

    fn run(&self, root: &Path, args: &[&str], input: Option<&str>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tlx"));
        command
            .args(args)
            .env("XDG_CONFIG_HOME", &self.config)
            .env("XDG_DATA_HOME", &self.data)
            .env("XDG_STATE_HOME", &self.state)
            .env("XDG_CACHE_HOME", &self.cache)
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env_remove("TELEX_SESSION_ID")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CODEX_THREAD_ID")
            .env_remove("DSH_SESSION_ID")
            .current_dir(root)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn tlx");
        if let Some(input) = input {
            child
                .stdin
                .as_mut()
                .expect("stdin")
                .write_all(input.as_bytes())
                .expect("stdin");
        }
        child.wait_with_output().expect("wait tlx")
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}
