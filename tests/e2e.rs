use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

const PASSWORD: &str = "admin-password-123";

struct Harness {
    _dir: TempDir,
    home: PathBuf,
    child: Child,
}

impl Harness {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        let init = run(&home, None, &["init", "--password-stdin"], Some(PASSWORD));
        assert!(init.status.success(), "init: {}", stderr(&init));

        let mut child = Command::new(bin());
        child.args(["maild", "--foreground"]);
        xdg_env(&mut child, &home);
        let child = child
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn maild");
        let socket = home.join("run/telex/maild.sock");
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if socket.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(socket.exists(), "maild socket not ready");
        Self {
            _dir: dir,
            home,
            child,
        }
    }

    fn run(&self, session: Option<&str>, args: &[&str], input: Option<&str>) -> Output {
        run(&self.home, session, args, input)
    }

    fn add_role(&self, address: &str) {
        let out = self.run(
            None,
            &["role", "add", address, "--password-stdin"],
            Some(PASSWORD),
        );
        assert!(out.status.success(), "role add {address}: {}", stderr(&out));
    }

    fn login(&self, session: &str, role: &str) -> String {
        let out = self.run(Some(session), &["login", role], None);
        assert!(out.status.success(), "login {role}: {}", stderr(&out));
        let text = stdout(&out);
        text.lines()
            .find_map(|line| line.strip_prefix("pairing code: "))
            .expect("pairing code")
            .trim()
            .to_string()
    }

    fn approve(&self, code: &str) {
        let out = self.run(None, &["approve", code, "--password-stdin"], Some(PASSWORD));
        assert!(out.status.success(), "approve: {}", stderr(&out));
    }

    fn bind(&self, session: &str, role: &str) {
        let code = self.login(session, role);
        self.approve(&code);
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_tlx"))
}

fn run(home: &Path, session: Option<&str>, args: &[&str], input: Option<&str>) -> Output {
    let mut command = Command::new(bin());
    command
        .args(args)
        .env("HOME", home)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("DSH_SESSION_ID")
        .env_remove("TELEX_SESSION_ID");
    if let Some(session) = session {
        command.env("TELEX_SESSION_ID", session);
    }
    command.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    xdg_env(&mut command, home);
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn tlx");
    if let Some(input) = input {
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(input.as_bytes())
            .expect("write stdin");
    }
    child.wait_with_output().expect("wait tlx")
}

fn xdg_env(command: &mut Command, root: &Path) {
    command
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_RUNTIME_DIR", root.join("run"));
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("expected json, got: {}\n{}", stdout(output), stderr(output)))
}

#[test]
fn send_read_ack_and_read_receipt() {
    let harness = Harness::new();
    harness.add_role("lead@local");
    harness.add_role("worker@local");
    harness.bind("session-lead", "lead@local");
    harness.bind("session-worker", "worker@local");

    let sent = harness.run(
        Some("session-lead"),
        &[
            "send",
            "--to",
            "worker@local",
            "--subject",
            "flaky test",
            "--body",
            "Can you look?",
            "--intent",
            "question",
            "--json",
        ],
        None,
    );
    assert!(sent.status.success(), "send: {}", stderr(&sent));
    let sent_value = json(&sent);
    let message_id = sent_value["message_id"]
        .as_str()
        .expect("message id")
        .to_string();

    let inbox = harness.run(Some("session-worker"), &["inbox", "--json"], None);
    assert!(inbox.status.success(), "inbox: {}", stderr(&inbox));
    let inbox_value = json(&inbox);
    assert_eq!(
        inbox_value["messages"].as_array().expect("messages").len(),
        1
    );
    assert_eq!(inbox_value["messages"][0]["unread"], true);

    let read = harness.run(
        Some("session-worker"),
        &["read", &message_id, "--json"],
        None,
    );
    assert!(read.status.success(), "read: {}", stderr(&read));
    assert_eq!(json(&read)["seen"], true);

    let sent_list = harness.run(Some("session-lead"), &["sent", "--json"], None);
    assert!(sent_list.status.success(), "sent: {}", stderr(&sent_list));
    let sent_value = json(&sent_list);
    assert_eq!(
        sent_value["messages"][0]["recipients"][0]["address"],
        "worker@local"
    );
    assert!(sent_value["messages"][0]["recipients"][0]["seen_at"].is_string());

    let ack = harness.run(Some("session-worker"), &["ack", &message_id], None);
    assert!(ack.status.success(), "ack: {}", stderr(&ack));
    let inbox_after = json(&harness.run(Some("session-worker"), &["inbox", "--json"], None));
    assert_eq!(inbox_after["messages"][0]["acked"], true);

    let sent_after = json(&harness.run(Some("session-lead"), &["sent", "--json"], None));
    assert!(sent_after["messages"][0]["recipients"][0]["acked_at"].is_string());
}

#[test]
fn binding_is_one_to_one_and_reassignment_revokes_old_session() {
    let harness = Harness::new();
    harness.add_role("worker@local");
    harness.bind("session-a", "worker@local");

    let old = harness.run(Some("session-a"), &["inbox"], None);
    assert!(
        old.status.success(),
        "old session should work: {}",
        stderr(&old)
    );

    let assign = harness.run(
        None,
        &[
            "admin",
            "assign",
            "worker@local",
            "session-b",
            "--harness",
            "codex",
            "--password-stdin",
        ],
        Some(PASSWORD),
    );
    assert!(assign.status.success(), "assign: {}", stderr(&assign));

    let old_after = harness.run(Some("session-a"), &["inbox"], None);
    assert!(!old_after.status.success(), "old session must be revoked");
    assert!(
        stderr(&old_after).contains("session_revoked"),
        "old session should be told it was revoked: {}",
        stderr(&old_after)
    );

    let new_session = harness.run(Some("session-b"), &["whoami"], None);
    assert!(
        new_session.status.success(),
        "new session: {}",
        stderr(&new_session)
    );
    assert!(stdout(&new_session).contains("worker@local"));
}

#[test]
fn hook_prints_one_line_badge() {
    let harness = Harness::new();
    harness.add_role("lead@local");
    harness.add_role("worker@local");
    harness.bind("session-lead", "lead@local");
    harness.bind("session-worker", "worker@local");

    let sent = harness.run(
        Some("session-lead"),
        &[
            "send",
            "--to",
            "worker@local",
            "--subject",
            "hi",
            "--body",
            "there",
            "--json",
        ],
        None,
    );
    assert!(sent.status.success(), "send: {}", stderr(&sent));

    let hook = harness.run(
        Some("session-worker"),
        &["hook", "--event", "UserPromptSubmit", "--harness", "claude"],
        Some(r#"{"session_id":"session-worker"}"#),
    );
    assert!(hook.status.success(), "hook: {}", stderr(&hook));
    let text = stdout(&hook);
    assert!(text.starts_with("[Telex]"), "badge: {text}");
    assert!(text.contains("1 封未读"));
}

#[test]
fn connect_claude_merges_without_clobbering_and_agents_md_is_idempotent() {
    let dir = TempDir::new().expect("temp");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let settings = dir.path().join("settings.json");
    std::fs::write(
        &settings,
        r#"{"model":"keep-me","hooks":{"UserPromptSubmit":[],"SessionStart":[{"hooks":[{"type":"command","command":"/bin/true"}]},{"hooks":[{"type":"command","command":"\"tlx\" hook --event SessionStart --harness claude"}]}]}}"#,
    )
    .expect("settings");
    let skill_file = home.join(".claude/skills/telex/SKILL.md");

    let out = run(
        &home,
        None,
        &[
            "connect",
            "claude",
            "--yes",
            "--events",
            "UserPromptSubmit,Stop",
            "--file",
            settings.to_str().unwrap(),
        ],
        None,
    );
    assert!(out.status.success(), "connect: {}", stderr(&out));
    let value: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).expect("read")).expect("json");
    assert_eq!(value["model"], "keep-me");
    let session_start = value["hooks"]["SessionStart"].as_array().expect("group");
    assert_eq!(session_start.len(), 1, "old Telex hook removed");
    assert_eq!(session_start[0]["hooks"][0]["command"], "/bin/true");
    for event in ["UserPromptSubmit", "Stop"] {
        let command = value["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap_or_default();
        assert!(
            command.contains(&format!("hook --event {event} --harness claude")),
            "{event}: {command}"
        );
    }
    let skill_text = std::fs::read_to_string(&skill_file).expect("skill file");
    assert!(
        skill_text.contains("name: telex"),
        "frontmatter: {skill_text}"
    );
    assert!(
        skill_text.contains("tlx inbox-line"),
        "instruction: {skill_text}"
    );

    let out = run(
        &home,
        None,
        &[
            "connect",
            "claude",
            "--yes",
            "--events",
            "UserPromptSubmit,Stop",
            "--file",
            settings.to_str().unwrap(),
        ],
        None,
    );
    assert!(out.status.success());
    let value: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).expect("read")).expect("json");
    let entries = value["hooks"]["UserPromptSubmit"]
        .as_array()
        .expect("hooks");
    let count = entries
        .iter()
        .filter(|entry| entry.to_string().contains("hook --event"))
        .count();
    assert_eq!(count, 1, "not idempotent");
    let skill_second = std::fs::read_to_string(&skill_file).expect("skill file");
    assert_eq!(skill_text, skill_second, "skill not idempotent");

    let agents = dir.path().join("AGENTS.md");
    let args = ["agents-md", "--write", "--file", agents.to_str().unwrap()];
    assert!(run(&home, None, &args, None).status.success());
    let first = std::fs::read_to_string(&agents).expect("agents");
    assert!(run(&home, None, &args, None).status.success());
    let second = std::fs::read_to_string(&agents).expect("agents");
    assert_eq!(first, second);
    assert_eq!(second.matches("<!-- telex:begin -->").count(), 1);
}

#[test]
fn admin_can_send_mail_without_a_bound_session() {
    let harness = Harness::new();
    harness.add_role("lead@local");
    harness.add_role("worker@local");
    harness.bind("session-worker", "worker@local");

    let sent = harness.run(
        None,
        &[
            "admin",
            "send",
            "--from",
            "lead@local",
            "--to",
            "worker@local",
            "--subject",
            "from admin",
            "--body",
            "hello",
            "--password-stdin",
            "--json",
        ],
        Some(PASSWORD),
    );
    assert!(sent.status.success(), "admin send: {}", stderr(&sent));
    assert_eq!(json(&sent)["delivered"][0], "worker@local");

    let inbox = json(&harness.run(Some("session-worker"), &["inbox", "--json"], None));
    assert_eq!(inbox["messages"][0]["from"], "lead@local");
}

#[test]
fn reply_all_keeps_cc_recipients_in_cc() {
    let harness = Harness::new();
    harness.add_role("lead@local");
    harness.add_role("worker@local");
    harness.add_role("observer@local");
    harness.bind("session-lead", "lead@local");
    harness.bind("session-worker", "worker@local");
    harness.bind("session-observer", "observer@local");

    let sent = harness.run(
        Some("session-lead"),
        &[
            "send",
            "--to",
            "worker@local",
            "--cc",
            "observer@local",
            "--subject",
            "status",
            "--body",
            "question",
            "--intent",
            "question",
            "--json",
        ],
        None,
    );
    assert!(sent.status.success(), "send: {}", stderr(&sent));
    let parent = json(&sent)["message_id"].as_str().unwrap().to_string();

    let reply = harness.run(
        Some("session-worker"),
        &["reply", &parent, "--all", "--body", "answer", "--json"],
        None,
    );
    assert!(reply.status.success(), "reply: {}", stderr(&reply));
    let reply_id = json(&reply)["message_id"].as_str().unwrap().to_string();

    let read = harness.run(
        Some("session-observer"),
        &["read", &reply_id, "--json"],
        None,
    );
    assert!(read.status.success(), "read: {}", stderr(&read));
    let envelope = &json(&read)["envelope"];
    assert_eq!(envelope["to"], serde_json::json!(["lead@local"]));
    assert_eq!(envelope["cc"], serde_json::json!(["observer@local"]));
}

#[test]
fn role_list_does_not_leak_session_ids() {
    let harness = Harness::new();
    let add = harness.run(
        None,
        &["role", "add", "private@local", "--password-stdin"],
        Some(PASSWORD),
    );
    assert!(add.status.success(), "role add: {}", stderr(&add));
    harness.bind("session-private", "private@local");

    let list = harness.run(Some("session-private"), &["role", "list", "--json"], None);
    assert!(list.status.success(), "role list: {}", stderr(&list));
    let role = &json(&list)["roles"][0];
    assert!(role.get("session_id").is_none());
    assert_eq!(role["address"], "private@local");

    let admin_list = harness.run(
        Some("session-private"),
        &["role", "list", "--json", "--password-stdin"],
        Some(PASSWORD),
    );
    assert!(
        admin_list.status.success(),
        "admin list: {}",
        stderr(&admin_list)
    );
    let role = &json(&admin_list)["roles"][0];
    assert_eq!(role["session_id"], "session-private");
}

#[test]
fn replying_to_your_own_sent_message_goes_to_the_recipient() {
    let harness = Harness::new();
    harness.add_role("lead@local");
    harness.add_role("worker@local");
    harness.bind("session-lead", "lead@local");
    harness.bind("session-worker", "worker@local");

    let sent = harness.run(
        Some("session-lead"),
        &[
            "send",
            "--to",
            "worker@local",
            "--subject",
            "follow up",
            "--body",
            "first",
            "--json",
        ],
        None,
    );
    assert!(sent.status.success(), "send: {}", stderr(&sent));
    let parent = json(&sent)["message_id"].as_str().unwrap().to_string();

    let reply = harness.run(
        Some("session-lead"),
        &["reply", &parent, "--body", "second"],
        None,
    );
    assert!(reply.status.success(), "reply: {}", stderr(&reply));

    let inbox = json(&harness.run(Some("session-worker"), &["inbox", "--json"], None));
    let subjects: Vec<&str> = inbox["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message["subject"].as_str())
        .collect();
    assert!(
        subjects.contains(&"Re: follow up"),
        "worker inbox: {subjects:?}"
    );
}

#[test]
fn read_escapes_terminal_control_characters() {
    let harness = Harness::new();
    harness.add_role("lead@local");
    harness.add_role("worker@local");
    harness.bind("session-lead", "lead@local");
    harness.bind("session-worker", "worker@local");

    let sent = harness.run(
        Some("session-lead"),
        &[
            "send",
            "--to",
            "worker@local",
            "--subject",
            "terminal",
            "--body",
            "before\u{1b}[2Jafter",
            "--json",
        ],
        None,
    );
    assert!(sent.status.success(), "send: {}", stderr(&sent));
    let message_id = json(&sent)["message_id"].as_str().unwrap().to_string();

    let read = harness.run(Some("session-worker"), &["read", &message_id], None);
    assert!(read.status.success(), "read: {}", stderr(&read));
    let text = stdout(&read);
    assert!(text.contains("\\u{001b}[2J"), "escaped body: {text}");
    assert!(!text.contains('\u{1b}'), "raw escape reached the terminal");
}

#[test]
fn init_rejects_an_invalid_domain() {
    let dir = TempDir::new().expect("temp");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let out = run(
        &home,
        None,
        &["init", "--domain", "bad domain", "--password-stdin"],
        Some(PASSWORD),
    );
    assert!(!out.status.success());
    assert!(stderr(&out).contains("invalid domain"));
}

#[test]
fn hook_stays_silent_for_unbound_session() {
    let harness = Harness::new();
    for event in ["UserPromptSubmit", "Stop"] {
        let out = harness.run(
            None,
            &["hook", "--event", event],
            Some(r#"{"session_id":"ghost-session"}"#),
        );
        assert!(out.status.success(), "hook {event}: {}", stderr(&out));
        assert_eq!(stdout(&out), "", "{event} should stay silent");
        assert_eq!(stderr(&out), "", "{event} should not write stderr");
    }
}

#[test]
fn hook_stays_silent_until_binding_is_approved() {
    let harness = Harness::new();
    harness.add_role("lead@local");
    harness.add_role("worker@local");
    harness.bind("session-lead", "lead@local");

    let code = harness.login("session-pending", "worker@local");
    let sent = harness.run(
        Some("session-lead"),
        &[
            "send",
            "--to",
            "worker@local",
            "--subject",
            "waiting",
            "--body",
            "approve me",
        ],
        None,
    );
    assert!(sent.status.success(), "send: {}", stderr(&sent));

    let before = harness.run(
        None,
        &["hook", "--event", "UserPromptSubmit"],
        Some(r#"{"session_id":"session-pending"}"#),
    );
    assert!(before.status.success());
    assert_eq!(stdout(&before), "", "pending session must stay silent");

    harness.approve(&code);
    let after = harness.run(
        None,
        &["hook", "--event", "UserPromptSubmit"],
        Some(r#"{"session_id":"session-pending"}"#),
    );
    assert!(after.status.success());
    assert!(
        stdout(&after).contains("1 封未读"),
        "approved session should get a badge: {}",
        stdout(&after)
    );
}

#[test]
fn hook_notifies_once_when_a_session_is_revoked() {
    let harness = Harness::new();
    harness.add_role("worker@local");
    harness.bind("session-worker", "worker@local");

    let logout = harness.run(Some("session-worker"), &["logout"], None);
    assert!(logout.status.success(), "logout: {}", stderr(&logout));

    let first = harness.run(
        None,
        &["hook", "--event", "UserPromptSubmit", "--harness", "claude"],
        Some(r#"{"session_id":"session-worker"}"#),
    );
    assert!(first.status.success());
    assert!(
        stdout(&first).contains("撤销"),
        "revoked notice: {}",
        stdout(&first)
    );

    let second = harness.run(
        None,
        &["hook", "--event", "UserPromptSubmit", "--harness", "claude"],
        Some(r#"{"session_id":"session-worker"}"#),
    );
    assert!(second.status.success());
    assert_eq!(stdout(&second), "", "notice should be one-shot");

    let whoami = harness.run(Some("session-worker"), &["whoami"], None);
    assert!(whoami.status.success());
    assert!(stdout(&whoami).contains("session revoked"));
}

#[test]
fn hook_event_name_from_payload_wins_over_flag() {
    let harness = Harness::new();
    harness.add_role("lead@local");
    harness.add_role("worker@local");
    harness.bind("session-lead", "lead@local");
    harness.bind("session-worker", "worker@local");

    let sent = harness.run(
        Some("session-lead"),
        &[
            "send",
            "--to",
            "worker@local",
            "--subject",
            "hi",
            "--body",
            "there",
            "--json",
        ],
        None,
    );
    assert!(sent.status.success(), "send: {}", stderr(&sent));

    // Flag says Stop, payload says UserPromptSubmit: payload wins and the
    // badge still prints.
    let out = harness.run(
        Some("session-worker"),
        &["hook", "--event", "Stop", "--harness", "claude"],
        Some(r#"{"session_id":"session-worker","hook_event_name":"UserPromptSubmit"}"#),
    );
    assert!(out.status.success(), "hook: {}", stderr(&out));
    assert!(stdout(&out).contains("1 封未读"), "badge: {}", stdout(&out));

    // Payload says Stop: exit code 2 with the badge on stderr.
    let out = harness.run(
        Some("session-worker"),
        &["hook", "--event", "UserPromptSubmit", "--harness", "claude"],
        Some(r#"{"session_id":"session-worker","hook_event_name":"Stop"}"#),
    );
    assert_eq!(out.status.code(), Some(2), "stop exit code");
    assert!(
        stderr(&out).contains("1 封未读"),
        "stop stderr: {}",
        stderr(&out)
    );
}

#[test]
fn connect_codex_writes_and_merges_idempotently() {
    let dir = TempDir::new().expect("temp");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let file = dir.path().join("hooks.json");
    std::fs::write(
        &file,
        r#"{"description":"keep me","hooks":{"Stop":[{"hooks":[{"type":"command","command":"/bin/true"}]}]}}"#,
    )
    .expect("seed");
    let skill_file = home.join(".codex/skills/telex/SKILL.md");

    let out = run(
        &home,
        None,
        &[
            "connect",
            "codex",
            "--yes",
            "--events",
            "UserPromptSubmit,Stop",
            "--file",
            file.to_str().unwrap(),
        ],
        None,
    );
    assert!(out.status.success(), "connect: {}", stderr(&out));
    assert!(
        stdout(&out).contains("/hooks"),
        "trust reminder printed: {}",
        stdout(&out)
    );
    let skill_text = std::fs::read_to_string(&skill_file).expect("skill file");
    assert!(
        skill_text.contains("name: telex"),
        "frontmatter: {skill_text}"
    );

    let value: Value =
        serde_json::from_str(&std::fs::read_to_string(&file).expect("read")).expect("json");
    assert_eq!(value["description"], "keep me", "description preserved");
    let stop_commands: Vec<&str> = value["hooks"]["Stop"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|group| group["hooks"].as_array())
        .flatten()
        .filter_map(|handler| handler["command"].as_str())
        .collect();
    assert!(stop_commands.contains(&"/bin/true"), "existing hook kept");

    let prompt_submit = value["hooks"]["UserPromptSubmit"]
        .as_array()
        .expect("group");
    let handler = &prompt_submit[0]["hooks"][0];
    assert_eq!(handler["type"], "command");
    assert_eq!(handler["timeout"], 10);
    assert!(
        handler["command"]
            .as_str()
            .unwrap()
            .contains("hook --event UserPromptSubmit --harness codex")
    );

    let stop_telex = value["hooks"]["Stop"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|group| group["hooks"].as_array())
        .flatten()
        .find(|handler| {
            handler["command"]
                .as_str()
                .unwrap_or("")
                .contains("hook --event Stop")
        })
        .expect("stop handler");
    assert_eq!(stop_telex["timeout"], 30);

    // Second run is idempotent.
    let out = run(
        &home,
        None,
        &[
            "connect",
            "codex",
            "--yes",
            "--events",
            "UserPromptSubmit,Stop",
            "--file",
            file.to_str().unwrap(),
        ],
        None,
    );
    assert!(out.status.success());
    let value: Value =
        serde_json::from_str(&std::fs::read_to_string(&file).expect("read")).expect("json");
    let telex_count = value["hooks"]["UserPromptSubmit"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|group| group["hooks"].as_array())
        .flatten()
        .filter_map(|handler| handler["command"].as_str())
        .filter(|command| command.contains("hook --event"))
        .count();
    assert_eq!(telex_count, 1, "not idempotent");
    let skill_second = std::fs::read_to_string(&skill_file).expect("skill file");
    assert_eq!(skill_text, skill_second, "skill not idempotent");
}

#[test]
fn connect_dsh_writes_idempotently() {
    let dir = TempDir::new().expect("temp");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let target = home.join(".dsh/skills/telex/SKILL.md");

    let out = run(&home, None, &["connect", "dsh", "--yes"], None);
    assert!(out.status.success(), "connect: {}", stderr(&out));

    let first = std::fs::read_to_string(&target).expect("skill file");
    assert!(
        first.starts_with("---\nname: telex\n"),
        "frontmatter: {first}"
    );
    assert!(first.contains("tlx inbox-line"), "instruction: {first}");

    let out = run(&home, None, &["connect", "dsh", "--yes"], None);
    assert!(out.status.success());
    let second = std::fs::read_to_string(&target).expect("skill file");
    assert_eq!(first, second, "not idempotent");
}

#[test]
fn connect_dsh_rejects_the_hook_file_flag() {
    let dir = TempDir::new().expect("temp");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let out = run(
        &home,
        None,
        &["connect", "dsh", "--file", "/tmp/hooks.json", "--yes"],
        None,
    );
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("--file is not supported"),
        "stderr: {}",
        stderr(&out)
    );
}

#[test]
fn connect_agy_writes_hook_and_skill() {
    let dir = TempDir::new().expect("temp");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let hooks = home.join(".gemini/config/hooks.json");
    std::fs::create_dir_all(hooks.parent().unwrap()).expect("hooks dir");
    std::fs::write(
        &hooks,
        r#"{"other":{"Stop":[{"type":"command","command":"/bin/true"}]}}"#,
    )
    .expect("seed");

    let out = run(&home, None, &["connect", "agy", "--yes"], None);
    assert!(out.status.success(), "connect: {}", stderr(&out));

    let value: Value =
        serde_json::from_str(&std::fs::read_to_string(&hooks).expect("read")).expect("json");
    assert_eq!(value["other"]["Stop"][0]["command"], "/bin/true");
    let command = value["telex"]["PreInvocation"][0]["command"]
        .as_str()
        .expect("command");
    assert!(command.contains("hook --event PreInvocation --harness agy"));
    let skill = home.join(".gemini/config/skills/telex/SKILL.md");
    assert!(skill.exists(), "skill missing");
    assert!(
        std::fs::read_to_string(skill)
            .unwrap()
            .contains("name: telex")
    );

    let out = run(&home, None, &["connect", "agy", "--yes"], None);
    assert!(out.status.success(), "second connect: {}", stderr(&out));
    let value: Value =
        serde_json::from_str(&std::fs::read_to_string(&hooks).expect("read")).expect("json");
    assert_eq!(value["telex"]["PreInvocation"].as_array().unwrap().len(), 1);
}

#[test]
fn agy_hook_returns_inject_steps() {
    let harness = Harness::new();
    harness.add_role("lead@local");
    harness.add_role("worker@local");
    harness.bind("session-lead", "lead@local");
    harness.bind("session-worker", "worker@local");
    let sent = harness.run(
        Some("session-lead"),
        &[
            "send",
            "--to",
            "worker@local",
            "--subject",
            "hi",
            "--body",
            "there",
            "--json",
        ],
        None,
    );
    assert!(sent.status.success(), "send: {}", stderr(&sent));

    let out = harness.run(
        None,
        &["hook", "--event", "PreInvocation", "--harness", "agy"],
        Some(r#"{"conversationId":"session-worker"}"#),
    );
    assert!(out.status.success(), "hook: {}", stderr(&out));
    let value: Value = serde_json::from_str(&stdout(&out)).expect("hook json");
    let message = value["injectSteps"][0]["systemMessage"]["systemMessage"]
        .as_str()
        .unwrap_or_default();
    assert!(message.contains("封未读"), "message: {message}");

    let unbound = harness.run(
        None,
        &["hook", "--event", "PreInvocation", "--harness", "agy"],
        Some(r#"{"conversationId":"ghost"}"#),
    );
    assert!(unbound.status.success());
    let value: Value = serde_json::from_str(&stdout(&unbound)).expect("unbound json");
    assert_eq!(value["injectSteps"], serde_json::json!([]));
}

#[test]
fn connect_requires_confirmation_in_a_pipe() {
    let dir = TempDir::new().expect("temp");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    let out = run(&home, None, &["connect", "dsh"], None);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("confirmation required"),
        "stderr: {}",
        stderr(&out)
    );
}

#[test]
fn connect_test_runs_a_synthetic_payload() {
    let harness = Harness::new();
    let out = harness.run(None, &["connect", "codex", "--test", "--yes"], None);
    assert!(out.status.success(), "connect test: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("event: UserPromptSubmit"),
        "event echoed: {text}"
    );
    assert!(
        text.contains("session: tlx-connect-test-codex"),
        "session: {text}"
    );
    assert!(text.contains("output: (none)"), "unbound outcome: {text}");
}

#[test]
fn setup_initializes_and_does_not_bind_a_session() {
    let dir = TempDir::new().expect("temp");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");

    // A harness session id must not turn setup into a binding operation.
    let out = run(
        &home,
        Some("setup-session"),
        &[
            "setup",
            "--password-stdin",
            "--domain",
            "local",
            "--role",
            "lead@local",
            "--json",
        ],
        Some(PASSWORD),
    );
    assert!(out.status.success(), "setup: {}", stderr(&out));
    let value = json(&out);
    assert_eq!(value["initialized"], true);
    assert_eq!(value["role"], "lead@local");
    assert!(value.get("session").is_none());
    assert!(value.get("bound").is_none());
    assert!(
        value["config"]
            .as_str()
            .unwrap()
            .ends_with("config/telex/config.json")
    );
    assert!(value["data"].as_str().unwrap().ends_with("data/telex"));

    let whoami = run(&home, Some("setup-session"), &["whoami"], None);
    assert!(whoami.status.success(), "whoami: {}", stderr(&whoami));
    assert!(
        stdout(&whoami).contains("not logged in"),
        "setup should not bind: {}",
        stdout(&whoami)
    );

    let again = run(&home, None, &["setup", "--json"], None);
    assert!(again.status.success(), "second setup: {}", stderr(&again));
    let value = json(&again);
    assert_eq!(value["initialized"], true);
    assert_eq!(value["domain"], "local");
    assert!(
        value["roles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|role| role == "lead@local")
    );
}
