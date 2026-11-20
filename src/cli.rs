//! Command line interface. All state lives behind the local daemon.

use std::io::{IsTerminal, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{Duration, Utc};
use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};

use crate::client::Client;
use crate::domain::{Address, Body, Envelope, Intent, MediaType};
use crate::paths::Paths;
use crate::store::Store;

#[derive(Parser)]
#[command(name = "tlx", version, about = "Telex agent mailbox")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create the mailbox home and set the admin password.
    Init(InitArgs),
    /// Set up a fresh mailbox in one step.
    Setup(SetupArgs),
    /// Show the current session and bound role.
    Whoami,
    /// Manage roles (the address book).
    Role(RoleCmd),
    /// Ask to bind this session to a role.
    Login(LoginArgs),
    /// Approve a pending login with the admin password.
    Approve(ApproveArgs),
    /// Administrative commands.
    Admin(AdminCmd),
    /// Revoke the current session token.
    Logout,
    /// Send a message.
    Send(SendArgs),
    /// Reply to a message.
    Reply(ReplyArgs),
    /// List the inbox.
    Inbox(InboxArgs),
    /// List sent messages with read receipts.
    Sent(InboxArgs),
    /// Read one message.
    Read(ReadArgs),
    /// Show a thread.
    Thread(ThreadArgs),
    /// Acknowledge a message.
    Ack(IdArgs),
    /// Claim a message before working on it.
    Claim(IdArgs),
    /// Release a claim.
    Release(IdArgs),
    /// Run a role's wake command.
    Wake(WakeArgs),
    /// Internal hook entry point. Reads hook JSON from stdin.
    #[command(hide = true)]
    Hook(HookArgs),
    /// Connect Telex to a harness, or show connection status.
    Connect(ConnectArgs),
    /// Write the AGENTS.md fallback block.
    AgentsMd(AgentsMdArgs),
    /// One-line unread summary.
    InboxLine(InboxLineArgs),
    /// Write the fallback inbox file.
    InboxFile(InboxFileArgs),
    /// Check spool health.
    Doctor(DoctorArgs),
    /// Index operations.
    Index(IndexCmd),
    /// Run the local daemon in the foreground.
    Maild(MaildArgs),
}

#[derive(Args)]
struct InitArgs {
    #[arg(long)]
    password_stdin: bool,
    #[arg(long, default_value = "local")]
    domain: String,
}

#[derive(Args)]
struct SetupArgs {
    #[arg(long)]
    password_stdin: bool,
    /// Default domain. Prompted when omitted in a terminal.
    #[arg(long)]
    domain: Option<String>,
    /// First role. Defaults to $USER@<domain>.
    #[arg(long)]
    role: Option<String>,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct RoleCmd {
    #[command(subcommand)]
    command: RoleSub,
}

#[derive(Subcommand)]
enum RoleSub {
    Add(RoleAddArgs),
    List(RoleListArgs),
    Delete(RoleDeleteArgs),
}

#[derive(Args)]
struct RoleAddArgs {
    address: String,
    #[arg(long)]
    display: Option<String>,
    /// JSON array, for example '["codex","queue","--thread","{session_id}"]'.
    #[arg(long)]
    wake_cmd: Option<String>,
    #[arg(long)]
    password_stdin: bool,
}

#[derive(Args)]
struct RoleListArgs {
    #[arg(long)]
    json: bool,
    #[arg(long)]
    password_stdin: bool,
}

#[derive(Args)]
struct RoleDeleteArgs {
    role: String,
    #[arg(long)]
    password_stdin: bool,
}

#[derive(Args)]
struct LoginArgs {
    role: String,
}

#[derive(Args)]
struct ApproveArgs {
    code: String,
    #[arg(long)]
    password_stdin: bool,
}

#[derive(Args)]
struct AdminCmd {
    #[command(subcommand)]
    command: AdminSub,
}

#[derive(Subcommand)]
enum AdminSub {
    Assign(AssignArgs),
    Requests(RequestsArgs),
    Passwd(PasswdArgs),
    Send(AdminSendArgs),
}

#[derive(Args)]
struct AssignArgs {
    role: String,
    session_id: String,
    #[arg(long, default_value = "unknown")]
    harness: String,
    #[arg(long)]
    password_stdin: bool,
}

#[derive(Args)]
struct RequestsArgs {
    #[arg(long)]
    json: bool,
    #[arg(long)]
    password_stdin: bool,
}

#[derive(Args)]
struct PasswdArgs {
    #[arg(long)]
    password_stdin: bool,
}

#[derive(Args)]
struct AdminSendArgs {
    #[arg(long)]
    from: String,
    #[command(flatten)]
    send: SendArgs,
    #[arg(long)]
    password_stdin: bool,
}

#[derive(Args)]
struct SendArgs {
    #[arg(long)]
    to: Vec<String>,
    #[arg(long)]
    cc: Vec<String>,
    #[arg(long)]
    bcc: Vec<String>,
    #[arg(long)]
    subject: String,
    #[arg(long)]
    body: Option<String>,
    #[arg(long)]
    body_file: Option<PathBuf>,
    #[arg(long, default_value = "note")]
    intent: String,
    #[arg(long, default_value_t = 1)]
    priority: u8,
    #[arg(long)]
    payload: Option<String>,
    #[arg(long)]
    expires_in: Option<String>,
    #[arg(long)]
    markdown: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct ReplyArgs {
    message_id: String,
    #[arg(long)]
    body: Option<String>,
    #[arg(long)]
    all: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct InboxArgs {
    #[arg(long)]
    json: bool,
    /// Include expired messages.
    #[arg(long)]
    all: bool,
}

#[derive(Args)]
struct ReadArgs {
    message_id: String,
    #[arg(long)]
    no_mark: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct ThreadArgs {
    thread_id: String,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct IdArgs {
    message_id: String,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct WakeArgs {
    role: Option<String>,
    #[arg(long)]
    password_stdin: bool,
}

#[derive(Args)]
struct HookArgs {
    #[arg(long, default_value = "SessionStart")]
    event: String,
    #[arg(long, default_value = "unknown")]
    harness: String,
}

#[derive(Args)]
struct ConnectArgs {
    /// claude, codex, or dsh. Omit to show connection status.
    harness: Option<String>,
    /// Write the configuration instead of printing it.
    #[arg(long)]
    write: bool,
    /// Hook events for Claude Code and Codex, comma separated.
    #[arg(long, default_value = "SessionStart")]
    events: String,
    /// Override the config file, or the skill file for dsh.
    #[arg(long)]
    file: Option<PathBuf>,
    /// Run a synthetic SessionStart payload after connecting.
    #[arg(long)]
    test: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct AgentsMdArgs {
    #[arg(long)]
    write: bool,
    #[arg(long, default_value = "AGENTS.md")]
    file: PathBuf,
}

#[derive(Args)]
struct InboxLineArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct InboxFileArgs {
    #[arg(long, default_value = "TELEX_INBOX.md")]
    out: PathBuf,
}

#[derive(Args)]
struct DoctorArgs {
    #[arg(long)]
    json: bool,
    #[arg(long)]
    password_stdin: bool,
}

#[derive(Args)]
struct IndexCmd {
    #[command(subcommand)]
    command: IndexSub,
}

#[derive(Subcommand)]
enum IndexSub {
    Rebuild,
}

#[derive(Args)]
struct MaildArgs {
    #[arg(long)]
    foreground: bool,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let home = Paths::from_env()?;
    match cli.command {
        Commands::Init(args) => init(&home, args),
        Commands::Setup(args) => setup(&home, args),
        Commands::Whoami => whoami(&home),
        Commands::Role(cmd) => role(&home, cmd),
        Commands::Login(args) => login(&home, args),
        Commands::Approve(args) => approve(&home, args),
        Commands::Admin(cmd) => admin(&home, cmd),
        Commands::Logout => logout(&home),
        Commands::Send(args) => send(&home, args),
        Commands::Reply(args) => reply(&home, args),
        Commands::Inbox(args) => list_folder(&home, "new", args),
        Commands::Sent(args) => list_folder(&home, "sent", args),
        Commands::Read(args) => read(&home, args),
        Commands::Thread(args) => thread(&home, args),
        Commands::Ack(args) => simple_rpc(
            &home,
            "mail.ack",
            json!({"message_id": args.message_id}),
            args.json,
        ),
        Commands::Claim(args) => simple_rpc(
            &home,
            "mail.claim",
            json!({"message_id": args.message_id}),
            args.json,
        ),
        Commands::Release(args) => simple_rpc(
            &home,
            "mail.release",
            json!({"message_id": args.message_id}),
            args.json,
        ),
        Commands::Wake(args) => wake(&home, args),
        Commands::Hook(args) => hook(&home, args),
        Commands::Connect(args) => connect(&home, args),
        Commands::AgentsMd(args) => agents_md(args),
        Commands::InboxLine(args) => inbox_line(&home, args),
        Commands::InboxFile(args) => inbox_file(&home, args),
        Commands::Doctor(args) => doctor(&home, args),
        Commands::Index(cmd) => index(&home, cmd),
        Commands::Maild(_) => crate::maild::run(home),
    }
}

fn session_id() -> Option<String> {
    Store::session_id_from_env()
}

fn harness() -> String {
    if std::env::var("CLAUDE_CODE_SESSION_ID").is_ok() {
        "claude".into()
    } else if std::env::var("CODEX_THREAD_ID").is_ok() {
        "codex".into()
    } else if std::env::var("DSH_SESSION_ID").is_ok() {
        "dsh".into()
    } else {
        "unknown".into()
    }
}

fn rpc(home: &Paths, method: &str, mut params: Value) -> Result<Value> {
    let store = Store::new(home.clone());
    let session = session_id();
    if let Some(session) = &session {
        params["session_id"] = json!(session);
    }
    let token = session
        .as_ref()
        .and_then(|session| store.read_token_file(session));
    Client::new(home.clone())
        .call(method, token.as_deref(), params)
        .map_err(|err| anyhow!("{}: {}", err.code, err.message))
}

fn rpc_admin(home: &Paths, method: &str, mut params: Value, password: &str) -> Result<Value> {
    params["password"] = json!(password);
    let client = Client::new(home.clone());
    client
        .call(method, None, params)
        .map_err(|err| anyhow!("{}: {}", err.code, err.message))
}

fn read_password(password_stdin: bool) -> Result<String> {
    if password_stdin || !std::io::stdin().is_terminal() {
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        let password = buffer.lines().next().unwrap_or("").trim().to_string();
        if password.is_empty() {
            bail!("empty password");
        }
        Ok(password)
    } else {
        Ok(rpassword::prompt_password("admin password: ")?)
    }
}

fn init(home: &Paths, args: InitArgs) -> Result<()> {
    let store = Store::new(home.clone());
    if store.config_path().exists() {
        bail!("already initialized: {}", home.data_dir().display());
    }
    let password = read_password(args.password_stdin)?;
    if password.chars().count() < 12 {
        bail!("admin password must be at least 12 characters");
    }
    let domain = args.domain.to_ascii_lowercase();
    Address::parse("probe", &domain).context("invalid domain")?;
    store.init(&password, &domain)?;
    println!("initialized {}", home.data_dir().display());
    println!("next: tlx role add <address> --password-stdin, then tlx login <role>");
    Ok(())
}

fn read_new_password(password_stdin: bool) -> Result<String> {
    if password_stdin || !std::io::stdin().is_terminal() {
        return read_password(true);
    }
    let first = rpassword::prompt_password("admin password: ")?;
    let second = rpassword::prompt_password("confirm password: ")?;
    if first != second {
        bail!("passwords do not match");
    }
    Ok(first)
}

fn prompt_line(prompt: &str, default: &str) -> Result<String> {
    if !std::io::stdin().is_terminal() {
        return Ok(default.to_string());
    }
    print!("{prompt} [{default}]: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let line = line.trim();
    Ok(if line.is_empty() {
        default.to_string()
    } else {
        line.to_string()
    })
}

fn default_role(domain: &str) -> String {
    let user = std::env::var("USER")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "admin".to_string());
    let candidate = format!("{user}@{domain}");
    Address::parse(&candidate, domain)
        .map(|address| address.to_string())
        .unwrap_or_else(|_| format!("admin@{domain}"))
}

fn setup(home: &Paths, args: SetupArgs) -> Result<()> {
    let store = Store::new(home.clone());
    if store.config_path().exists() {
        let config = store.load_config()?;
        let roles = store.list_roles()?;
        let role_names: Vec<&str> = roles.iter().map(|role| role.address.as_str()).collect();
        if args.json {
            println!(
                "{}",
                json!({
                    "initialized": true,
                    "config": home.config_file().display().to_string(),
                    "data": home.data_dir().display().to_string(),
                    "domain": config.domain,
                    "roles": role_names,
                })
            );
        } else {
            println!("already initialized: {}", home.data_dir().display());
            println!("domain: {}", config.domain);
            if role_names.is_empty() {
                println!("roles: (none)");
            } else {
                println!("roles: {}", role_names.join(", "));
            }
        }
        return Ok(());
    }

    let password = read_new_password(args.password_stdin)?;
    if password.chars().count() < 12 {
        bail!("admin password must be at least 12 characters");
    }
    let domain = match args.domain {
        Some(domain) => domain.to_ascii_lowercase(),
        None => prompt_line("domain", "local")?.to_ascii_lowercase(),
    };
    Address::parse("probe", &domain).context("invalid domain")?;
    let role_default = default_role(&domain);
    let role_input = match args.role {
        Some(role) => role,
        None => prompt_line("first role", &role_default)?,
    };
    let role_address = Address::parse(&role_input, &domain)?;

    store.init(&password, &domain)?;
    let role = store.add_role(&role_address, None, Vec::new())?;

    if args.json {
        println!(
            "{}",
            json!({
                "initialized": true,
                "config": home.config_file().display().to_string(),
                "data": home.data_dir().display().to_string(),
                "domain": domain,
                "role": role.address,
            })
        );
    } else {
        println!("initialized: {}", home.data_dir().display());
        println!("domain: {domain}");
        println!("role: {}", role.address);
        println!("next:");
        println!("  tlx connect claude --write");
        println!("  tlx role add worker@local --password-stdin");
        println!(
            "  在 harness 会话里运行 tlx login {}，再在终端运行 tlx approve <配对码>",
            role.address
        );
    }
    Ok(())
}

fn whoami(home: &Paths) -> Result<()> {
    let session = session_id();
    match rpc(home, "whoami", json!({})) {
        Ok(value) => println!("{}", serde_json::to_string_pretty(&value)?),
        Err(err) => {
            if let Some(session) = session {
                println!("session: {session}");
                println!("role: not logged in ({err})");
            } else {
                println!("no session id in environment");
            }
        }
    }
    Ok(())
}

fn role(home: &Paths, cmd: RoleCmd) -> Result<()> {
    match cmd.command {
        RoleSub::Add(args) => {
            let password = read_password(args.password_stdin)?;
            let wake_cmd: Option<Value> = match args.wake_cmd {
                Some(raw) => {
                    Some(serde_json::from_str(&raw).context("wake-cmd must be a JSON array")?)
                }
                None => None,
            };
            let value = rpc_admin(
                home,
                "role.add",
                json!({"address": args.address, "display": args.display, "wake_cmd": wake_cmd}),
                &password,
            )?;
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        RoleSub::List(args) => {
            let value = if args.password_stdin {
                let password = read_password(true)?;
                rpc_admin(home, "role.list", json!({}), &password)?
            } else if let Ok(value) = rpc(home, "role.list", json!({})) {
                value
            } else {
                let password = read_password(false)?;
                rpc_admin(home, "role.list", json!({}), &password)?
            };
            if args.json {
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                for role in value["roles"].as_array().cloned().unwrap_or_default() {
                    let address = role["address"].as_str().unwrap_or("");
                    let display = role["display"].as_str().unwrap_or("");
                    match role["session_id"].as_str() {
                        Some(session) => println!("{address}\t{display}\t{session}"),
                        None => println!("{address}\t{display}"),
                    }
                }
            }
        }
        RoleSub::Delete(args) => {
            let password = read_password(args.password_stdin)?;
            let value = rpc_admin(home, "role.delete", json!({"role": args.role}), &password)?;
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
    }
    Ok(())
}

fn login(home: &Paths, args: LoginArgs) -> Result<()> {
    let session = session_id().ok_or_else(|| anyhow!("no session id in environment"))?;
    let domain = Store::new(home.clone()).config().domain;
    let role = Address::parse(&args.role, &domain)?.to_string();
    let value = rpc(
        home,
        "login",
        json!({"role": role, "session_id": session, "harness": harness()}),
    )?;
    println!("pairing code: {}", value["code"].as_str().unwrap_or("?"));
    println!("ask the admin to run: tlx approve <code>");
    Ok(())
}

fn approve(home: &Paths, args: ApproveArgs) -> Result<()> {
    let password = read_password(args.password_stdin)?;
    let value = rpc_admin(home, "approve", json!({"code": args.code}), &password)?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn admin(home: &Paths, cmd: AdminCmd) -> Result<()> {
    match cmd.command {
        AdminSub::Assign(args) => {
            let password = read_password(args.password_stdin)?;
            let domain = Store::new(home.clone()).config().domain;
            let role = Address::parse(&args.role, &domain)?.to_string();
            let value = rpc_admin(
                home,
                "assign",
                json!({"role": role, "session_id": args.session_id, "harness": args.harness}),
                &password,
            )?;
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        AdminSub::Requests(args) => {
            let password = read_password(args.password_stdin)?;
            let value = rpc_admin(home, "requests", json!({}), &password)?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                for request in value["requests"].as_array().cloned().unwrap_or_default() {
                    println!(
                        "{}\t{}\t{}",
                        request["code"].as_str().unwrap_or(""),
                        request["role"].as_str().unwrap_or(""),
                        request["session_id"].as_str().unwrap_or("")
                    );
                }
            }
        }
        AdminSub::Send(args) => {
            let password = read_password(args.password_stdin)?;
            let domain = Store::new(home.clone()).config().domain;
            let from = Address::parse(&args.from, &domain)?;
            let envelope = build_envelope(from, &domain, &args.send)?;
            let value = rpc_admin(
                home,
                "admin.send",
                json!({"envelope": envelope, "from": args.from.clone()}),
                &password,
            )?;
            print_send_result(&value, args.send.json);
        }
        AdminSub::Passwd(args) => {
            let (old, new) = if args.password_stdin || !std::io::stdin().is_terminal() {
                let mut buffer = String::new();
                std::io::stdin().read_to_string(&mut buffer)?;
                let mut lines = buffer.lines();
                let old = lines.next().unwrap_or("").trim().to_string();
                let new = lines.next().unwrap_or("").trim().to_string();
                (old, new)
            } else {
                let old = rpassword::prompt_password("current password: ")?;
                let new = rpassword::prompt_password("new password: ")?;
                (old, new)
            };
            if new.chars().count() < 12 {
                bail!("admin password must be at least 12 characters");
            }
            let value = rpc_admin(home, "admin.passwd", json!({"new_password": new}), &old)?;
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
    }
    Ok(())
}

fn logout(home: &Paths) -> Result<()> {
    let value = rpc(home, "logout", json!({}))?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn parse_intent(raw: &str) -> Result<Intent> {
    Ok(match raw {
        "note" => Intent::Note,
        "status" => Intent::Status,
        "alert" => Intent::Alert,
        "question" => Intent::Question,
        "review-request" => Intent::ReviewRequest,
        "handoff" => Intent::Handoff,
        "answer" => Intent::Answer,
        "ack" => Intent::Ack,
        other => bail!("unknown intent: {other}"),
    })
}

fn parse_duration(raw: &str) -> Result<Duration> {
    let mut chars = raw.chars();
    let unit = chars.next_back().context("empty duration")?;
    let number: i64 = chars.as_str().parse().context("bad duration")?;
    if number <= 0 {
        bail!("duration must be positive: {raw}");
    }
    Ok(match unit {
        'm' => Duration::minutes(number),
        'h' => Duration::hours(number),
        'd' => Duration::days(number),
        _ => bail!("bad duration: {raw}"),
    })
}

fn read_body(body: Option<String>, body_file: Option<PathBuf>) -> Result<String> {
    if let Some(body) = body {
        return Ok(body);
    }
    if let Some(path) = body_file {
        return Ok(std::fs::read_to_string(path)?);
    }
    let mut buffer = String::new();
    std::io::stdin().read_to_string(&mut buffer)?;
    Ok(buffer)
}

fn current_role(home: &Paths) -> Result<Address> {
    let value = rpc(home, "whoami", json!({}))?;
    let role = value["role"]
        .as_str()
        .ok_or_else(|| anyhow!("not logged in"))?;
    Ok(Address::parse(role, "local")?)
}

fn build_envelope(from: Address, domain: &str, args: &SendArgs) -> Result<Envelope> {
    let body = read_body(args.body.clone(), args.body_file.clone())?;
    let mut envelope = Envelope::new(
        from,
        args.to
            .iter()
            .map(|value| Address::parse(value, domain).map_err(anyhow::Error::from))
            .collect::<Result<Vec<_>>>()?,
        args.subject.clone(),
        &body,
        parse_intent(&args.intent)?,
    );
    envelope.cc = args
        .cc
        .iter()
        .map(|value| Address::parse(value, domain).map_err(anyhow::Error::from))
        .collect::<Result<Vec<_>>>()?;
    envelope.bcc = args
        .bcc
        .iter()
        .map(|value| Address::parse(value, domain).map_err(anyhow::Error::from))
        .collect::<Result<Vec<_>>>()?;
    envelope.priority = args.priority;
    if let Some(raw) = &args.payload {
        let payload: serde_json::Value =
            serde_json::from_str(raw).context("payload must be JSON")?;
        envelope.payload = Some(
            payload
                .as_object()
                .cloned()
                .ok_or_else(|| anyhow!("payload must be a JSON object"))?,
        );
    }
    if let Some(raw) = &args.expires_in {
        envelope.expires_at = Some(Utc::now() + parse_duration(raw)?);
    }
    if args.markdown {
        envelope.body = Body::from_text_with(&body, MediaType::TextMarkdown);
    }
    envelope.dedupe_key = envelope.compute_dedupe_key();
    Ok(envelope)
}

fn print_send_result(value: &Value, json_output: bool) {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_default()
        );
    } else {
        println!("sent {}", value["message_id"].as_str().unwrap_or("?"));
        if let Some(delivered) = value["delivered"].as_array() {
            println!(
                "delivered: {}",
                delivered
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
}

fn send(home: &Paths, args: SendArgs) -> Result<()> {
    let from = current_role(home)?;
    let domain = Store::new(home.clone()).config().domain;
    let envelope = build_envelope(from, &domain, &args)?;
    let value = rpc(home, "mail.send", json!({"envelope": envelope}))?;
    print_send_result(&value, args.json);
    Ok(())
}

fn reply(home: &Paths, args: ReplyArgs) -> Result<()> {
    let from = current_role(home)?;
    let parent_value = rpc(
        home,
        "mail.read",
        json!({"message_id": args.message_id, "mark_seen": false}),
    )?;
    let parent: Envelope = serde_json::from_value(parent_value["envelope"].clone())?;
    let mut to = if parent.from.address == from {
        parent.to.clone()
    } else {
        vec![parent.from.address.clone()]
    };
    to.retain(|address| address != &from);
    if to.is_empty() {
        to.push(parent.from.address.clone());
    }
    let mut cc = Vec::new();
    if args.all {
        for address in &parent.to {
            if address != &from && !to.contains(address) {
                to.push(address.clone());
            }
        }
        for address in &parent.cc {
            if address != &from && !to.contains(address) && !cc.contains(address) {
                cc.push(address.clone());
            }
        }
    }
    let body = read_body(args.body, None)?;
    let mut envelope = Envelope::reply_to(&parent, from, to, &body);
    envelope.cc = cc;
    envelope.dedupe_key = envelope.compute_dedupe_key();
    let value = rpc(home, "mail.send", json!({"envelope": envelope}))?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("replied {}", value["message_id"].as_str().unwrap_or("?"));
    }
    Ok(())
}

fn list_folder(home: &Paths, folder: &str, args: InboxArgs) -> Result<()> {
    let value = rpc(home, "mail.list", json!({"folder": folder}))?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    let messages = value["messages"].as_array().cloned().unwrap_or_default();
    let unread = messages
        .iter()
        .filter(|entry| entry["unread"].as_bool().unwrap_or(false))
        .count();
    println!(
        "{} ({} unread / {})",
        if folder == "sent" { "SENT" } else { "INBOX" },
        unread,
        messages.len()
    );
    if !args.all {
        let expired = messages
            .iter()
            .filter(|entry| entry["expired"].as_bool().unwrap_or(false))
            .count();
        if expired > 0 {
            println!("{expired} expired message(s) hidden; use --all to show them");
        }
    }
    for entry in messages {
        if !args.all && entry["expired"].as_bool().unwrap_or(false) {
            continue;
        }
        let flag = if entry["unread"].as_bool().unwrap_or(false) {
            "●"
        } else {
            "○"
        };
        let ack = if entry["acked"].as_bool().unwrap_or(false) {
            "✓"
        } else {
            " "
        };
        let status_suffix = if folder == "sent" {
            let recipients = entry["recipients"].as_array().cloned().unwrap_or_default();
            if recipients.is_empty() {
                String::new()
            } else {
                let delivered = recipients
                    .iter()
                    .filter(|item| item["delivered"].as_bool().unwrap_or(false))
                    .count();
                let read = recipients
                    .iter()
                    .filter_map(|item| {
                        item["seen_at"]
                            .as_str()
                            .map(|at| format!("{}@{}", item["address"].as_str().unwrap_or(""), at))
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                let acked = recipients
                    .iter()
                    .filter_map(|item| {
                        item["acked_at"]
                            .as_str()
                            .map(|at| format!("{}@{}", item["address"].as_str().unwrap_or(""), at))
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    " delivered:{delivered}/{} read:[{read}] acked:[{acked}]",
                    recipients.len()
                )
            }
        } else {
            String::new()
        };
        println!(
            "{flag}{ack} {} {} {} | {} [{}, p{}]{status_suffix}",
            entry["message_id"].as_str().unwrap_or(""),
            entry["created_at"].as_str().unwrap_or(""),
            entry["from"].as_str().unwrap_or(""),
            entry["subject"].as_str().unwrap_or(""),
            entry["intent"].as_str().unwrap_or(""),
            entry["priority"].as_u64().unwrap_or(1)
        );
    }
    Ok(())
}

fn read(home: &Paths, args: ReadArgs) -> Result<()> {
    let value = rpc(
        home,
        "mail.read",
        json!({"message_id": args.message_id, "mark_seen": !args.no_mark}),
    )?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    let envelope = &value["envelope"];
    println!(
        "From:     {}",
        envelope["from"]["address"].as_str().unwrap_or("")
    );
    for (label, key) in [
        ("To:       ", "to"),
        ("Cc:       ", "cc"),
        ("Bcc:      ", "bcc"),
    ] {
        let value = envelope[key]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        if !value.is_empty() {
            println!("{label}{value}");
        }
    }
    println!("Subject:  {}", envelope["subject"].as_str().unwrap_or(""));
    println!(
        "Date:     {}",
        envelope["created_at"].as_str().unwrap_or("")
    );
    println!(
        "Message:  {}",
        envelope["message_id"].as_str().unwrap_or("")
    );
    println!("Thread:   {}", envelope["thread_id"].as_str().unwrap_or(""));
    println!("Intent:   {}", envelope["intent"].as_str().unwrap_or(""));
    println!(
        "Seen: {}  Acked: {}",
        value["seen"].as_bool().unwrap_or(false),
        value["acked"].as_bool().unwrap_or(false)
    );
    println!();
    println!("{}", terminal_text(value["body"].as_str().unwrap_or("")));
    Ok(())
}

fn terminal_text(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '\n' | '\t' => out.push(ch),
            c if c.is_control() => out.push_str(&format!("\\u{{{:04x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn thread(home: &Paths, args: ThreadArgs) -> Result<()> {
    let value = rpc(home, "mail.thread", json!({"thread_id": args.thread_id}))?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    for entry in value["messages"].as_array().cloned().unwrap_or_default() {
        println!(
            "{}  {}  {} | {}",
            entry["created_at"].as_str().unwrap_or(""),
            entry["from"].as_str().unwrap_or(""),
            entry["message_id"].as_str().unwrap_or(""),
            entry["subject"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

fn simple_rpc(home: &Paths, method: &str, params: Value, json_output: bool) -> Result<()> {
    let value = rpc(home, method, params)?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("ok");
    }
    Ok(())
}

fn wake(home: &Paths, args: WakeArgs) -> Result<()> {
    let role = match args.role {
        Some(role) => role,
        None => {
            let value = rpc(home, "whoami", json!({}))?;
            value["role"].as_str().unwrap_or("").to_string()
        }
    };
    if role.is_empty() {
        bail!("no role");
    }
    let params = if args.password_stdin {
        let password = read_password(true)?;
        json!({"role": role, "password": password})
    } else {
        json!({"role": role})
    };
    let value = rpc(home, "wake", params)?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

enum HookEventKind {
    SessionStart,
    UserPromptSubmit,
    Stop,
    Other,
}

fn hook_event_kind(name: &str) -> HookEventKind {
    match name.to_ascii_lowercase().as_str() {
        "sessionstart" => HookEventKind::SessionStart,
        "userpromptsubmit" => HookEventKind::UserPromptSubmit,
        "stop" => HookEventKind::Stop,
        _ => HookEventKind::Other,
    }
}

struct HookOutcome {
    stdout: Vec<String>,
    stop_stderr: Option<String>,
}

impl HookOutcome {
    fn empty() -> Self {
        Self {
            stdout: Vec::new(),
            stop_stderr: None,
        }
    }
}

fn hook_session(payload: &Value) -> Option<String> {
    payload["session_id"]
        .as_str()
        .or_else(|| payload["thread_id"].as_str())
        .map(|value| value.to_string())
        .or_else(session_id)
}

/// Compute what the hook should print for one payload. Shared by the stdin
/// entry point and `tlx connect --test`, so the fixture path exercises the same
/// code as a real harness invocation.
fn evaluate_hook(home: &Paths, payload: &Value, event: &str) -> HookOutcome {
    let kind = hook_event_kind(event);
    let Some(session) = hook_session(payload) else {
        return HookOutcome::empty();
    };
    let store = Store::new(home.clone());
    if !store.config_path().exists() {
        return HookOutcome::empty();
    }
    // A hook is inert until the session has been explicitly joined and the
    // admin has approved it. Pending logins and unrelated harness sessions
    // both have no token file, so they stay silent.
    let Some(token) = store.read_token_file(&session) else {
        return HookOutcome::empty();
    };
    let Ok(value) =
        Client::new(home.clone()).call("mail.unread", Some(&token), json!({"session_id": session}))
    else {
        return HookOutcome::empty();
    };
    let count = value["count"].as_u64().unwrap_or(0);
    if count == 0 {
        return HookOutcome::empty();
    }
    let role = value["role"].as_str().unwrap_or("");
    let line = format!("[Telex] 你有 {count} 封未读（{role}）。查看：`tlx inbox`。");
    match kind {
        HookEventKind::Stop => {
            if hook_budget(&store, role, count) {
                HookOutcome {
                    stdout: Vec::new(),
                    stop_stderr: Some(line),
                }
            } else {
                HookOutcome::empty()
            }
        }
        HookEventKind::SessionStart | HookEventKind::UserPromptSubmit => HookOutcome {
            stdout: vec![line],
            stop_stderr: None,
        },
        HookEventKind::Other => HookOutcome::empty(),
    }
}

fn hook(home: &Paths, args: HookArgs) -> Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).ok();
    let payload: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let event = payload["hook_event_name"]
        .as_str()
        .unwrap_or(&args.event)
        .to_string();
    let outcome = evaluate_hook(home, &payload, &event);
    for line in &outcome.stdout {
        println!("{line}");
    }
    if let Some(stderr_line) = outcome.stop_stderr {
        eprintln!("{stderr_line}");
        std::process::exit(2);
    }
    Ok(())
}

fn connect(home: &Paths, args: ConnectArgs) -> Result<()> {
    let Some(raw_harness) = args.harness.as_deref() else {
        if args.write || args.test {
            bail!("name a harness: claude, codex, or dsh");
        }
        return connect_status(home, args.json);
    };
    let harness = raw_harness.to_ascii_lowercase();
    match harness.as_str() {
        "claude" | "codex" => {
            if args.write || !args.test {
                connect_hook(&harness, &args)?;
            }
            if args.test {
                connect_test(home, &harness);
            }
        }
        "dsh" => {
            if args.events != "SessionStart" {
                bail!("dsh uses a skill, not hook events");
            }
            if args.test {
                bail!("--test applies to claude and codex hooks, not the dsh skill");
            }
            connect_skill(&args)?;
        }
        other => bail!("unknown harness: {other}. Use claude, codex, or dsh"),
    }
    Ok(())
}

fn connect_test(home: &Paths, harness: &str) {
    let session = format!("tlx-connect-test-{harness}");
    let payload = json!({
        "session_id": session,
        "hook_event_name": "SessionStart",
        "cwd": ".",
    });
    let outcome = evaluate_hook(home, &payload, "SessionStart");
    println!("event: SessionStart");
    println!("session: {session}");
    if outcome.stdout.is_empty() && outcome.stop_stderr.is_none() {
        println!("output: (none)");
    }
    for line in &outcome.stdout {
        println!("stdout: {line}");
    }
    if let Some(stderr_line) = &outcome.stop_stderr {
        println!("stderr: {stderr_line}");
        println!("exit code: 2");
    }
}

struct HookConfigStatus {
    name: &'static str,
    path: PathBuf,
    installed: bool,
    binary: Option<String>,
    binary_ok: bool,
}

fn read_hook_config_status(name: &'static str, path: &Path) -> HookConfigStatus {
    let mut status = HookConfigStatus {
        name,
        path: path.to_path_buf(),
        installed: false,
        binary: None,
        binary_ok: false,
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return status;
    };
    let Ok(doc) = serde_json::from_str::<Value>(&text) else {
        return status;
    };
    let Some(hooks) = doc["hooks"].as_object() else {
        return status;
    };
    for entries in hooks.values() {
        let Some(groups) = entries.as_array() else {
            continue;
        };
        for group in groups {
            let Some(handlers) = group["hooks"].as_array() else {
                continue;
            };
            for handler in handlers {
                let Some(command) = handler["command"].as_str() else {
                    continue;
                };
                if !command.contains("hook --event") {
                    continue;
                }
                status.installed = true;
                if status.binary.is_none() {
                    status.binary = extract_command_binary(command);
                    status.binary_ok = status
                        .binary
                        .as_ref()
                        .is_some_and(|bin| Path::new(bin).exists());
                }
            }
        }
    }
    status
}

fn extract_command_binary(command: &str) -> Option<String> {
    let trimmed = command.trim();
    if let Some(rest) = trimmed.strip_prefix('"') {
        return rest.split('"').next().map(|value| value.to_string());
    }
    trimmed
        .split_whitespace()
        .next()
        .map(|value| value.to_string())
}

fn describe_hook_config(status: &HookConfigStatus) -> String {
    if !status.path.exists() || !status.installed {
        format!(
            "{}: {}（未安装，运行 tlx connect {} --write）",
            status.name,
            status.path.display(),
            status.name
        )
    } else if !status.binary_ok {
        format!(
            "{}: 已安装，但命令里的 tlx 路径失效（{}）",
            status.name,
            status.binary.clone().unwrap_or_default()
        )
    } else {
        format!(
            "{}: 已安装（tlx: {}）",
            status.name,
            status.binary.clone().unwrap_or_default()
        )
    }
}

fn connect_status(home: &Paths, json_output: bool) -> Result<()> {
    let user_home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()));
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home.join(".codex"));
    let claude = read_hook_config_status("claude", &user_home.join(".claude/settings.json"));
    let codex = read_hook_config_status("codex", &codex_home.join("hooks.json"));
    let dsh_skill_path = user_home.join(".dsh/skills/telex/SKILL.md");
    let dsh_installed = dsh_skill_path.exists();
    let maild_ok = Client::new(home.clone())
        .call("hello", None, json!({}))
        .is_ok();
    let initialized = Store::new(home.clone()).config_path().exists();

    if json_output {
        let value = json!({
            "claude": {
                "path": claude.path.display().to_string(),
                "installed": claude.installed,
                "binary": claude.binary,
                "binary_ok": claude.binary_ok,
            },
            "codex": {
                "path": codex.path.display().to_string(),
                "installed": codex.installed,
                "binary": codex.binary,
                "binary_ok": codex.binary_ok,
            },
            "dsh_skill": {
                "path": dsh_skill_path.display().to_string(),
                "installed": dsh_installed,
            },
            "maild_running": maild_ok,
            "initialized": initialized,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    println!("{}", describe_hook_config(&claude));
    println!("{}", describe_hook_config(&codex));
    if dsh_installed {
        println!("dsh skill: {}（已安装）", dsh_skill_path.display());
    } else {
        println!(
            "dsh skill: {}（未安装，运行 tlx connect dsh --write）",
            dsh_skill_path.display()
        );
    }
    println!("maild: {}", if maild_ok { "运行中" } else { "未运行" });
    println!(
        "telex data: {}{}",
        home.data_dir().display(),
        if initialized {
            ""
        } else {
            "（未初始化，运行 tlx init）"
        }
    );
    Ok(())
}

fn hook_budget(store: &Store, role: &str, _count: u64) -> bool {
    let path = store
        .paths()
        .hook_dir()
        .join(format!("{}.json", role.replace('@', "_at_")));
    let now = Utc::now().timestamp_millis();
    let mut stops: Vec<i64> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Vec<i64>>(&text).ok())
        .unwrap_or_default();
    stops.retain(|ts| now - *ts < 30 * 60 * 1000);
    if stops.last().is_some_and(|last| now - *last < 60_000) || stops.len() >= 3 {
        return false;
    }
    stops.push(now);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&path, serde_json::to_string(&stops).unwrap_or_default()).ok();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).ok();
    true
}

fn hook_command_path() -> String {
    std::env::current_exe()
        .map(|path| format!("\"{}\"", path.display()))
        .unwrap_or_else(|_| "tlx".to_string())
}

fn connect_hook(harness: &str, args: &ConnectArgs) -> Result<()> {
    let user_home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()));
    let default_file = match harness {
        "claude" => user_home.join(".claude/settings.json"),
        _ => std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| user_home.join(".codex"))
            .join("hooks.json"),
    };
    let mut hooks = serde_json::Map::new();
    for event in args
        .events
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let template = hook_template(harness, event)?;
        let items = template
            .get("hooks")
            .cloned()
            .ok_or_else(|| anyhow!("hook template for {event} has no hooks array"))?;
        hooks.insert(event.to_string(), items);
    }
    let settings = json!({"hooks": hooks});
    if !args.write {
        println!("{}", serde_json::to_string_pretty(&settings)?);
        return Ok(());
    }
    let file = args.file.clone().unwrap_or(default_file);
    let mut doc: Value = std::fs::read_to_string(&file)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| json!({}));
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if file.exists() {
        std::fs::write(file.with_extension("json.bak"), std::fs::read(&file)?).ok();
    }
    let object = doc
        .as_object_mut()
        .ok_or_else(|| anyhow!("settings file is not an object"))?;
    let hooks_object = object.entry("hooks").or_insert_with(|| json!({}));
    let hooks_object = hooks_object
        .as_object_mut()
        .ok_or_else(|| anyhow!("hooks is not an object"))?;
    for (event, value) in settings["hooks"].as_object().unwrap() {
        let existing = hooks_object
            .entry(event.clone())
            .or_insert_with(|| json!([]));
        let entries = existing
            .as_array_mut()
            .ok_or_else(|| anyhow!("hook entry is not an array"))?;
        entries.retain(|entry| !entry.to_string().contains("hook --event"));
        if let Some(items) = value.as_array() {
            entries.extend(items.clone());
        }
    }
    std::fs::write(&file, serde_json::to_string_pretty(&doc)? + "\n")?;
    println!("connected {harness}: {}", file.display());
    if harness == "codex" {
        println!("启动 codex 后运行 /hooks 审核并信任 Telex 的 hook，否则不会执行。");
    }
    Ok(())
}

fn hook_template(harness: &str, event: &str) -> Result<Value> {
    let file = match event {
        "SessionStart" => "session-start",
        "UserPromptSubmit" => "user-prompt-submit",
        "Stop" => "stop",
        other => bail!("unsupported hook event: {other}"),
    };
    let raw = match (harness, file) {
        ("claude", "session-start") => include_str!("../assets/hooks/claude/session-start.json"),
        ("claude", "user-prompt-submit") => {
            include_str!("../assets/hooks/claude/user-prompt-submit.json")
        }
        ("claude", "stop") => include_str!("../assets/hooks/claude/stop.json"),
        ("codex", "session-start") => include_str!("../assets/hooks/codex/session-start.json"),
        ("codex", "user-prompt-submit") => {
            include_str!("../assets/hooks/codex/user-prompt-submit.json")
        }
        ("codex", "stop") => include_str!("../assets/hooks/codex/stop.json"),
        _ => bail!("unsupported hook template: {harness}/{file}"),
    };
    let mut value: Value = serde_json::from_str(raw)?;
    replace_placeholder(&mut value, "{{TLX}}", &hook_command_path());
    Ok(value)
}

fn replace_placeholder(value: &mut Value, needle: &str, replacement: &str) {
    match value {
        Value::String(text) => *text = text.replace(needle, replacement),
        Value::Array(items) => {
            for item in items {
                replace_placeholder(item, needle, replacement);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                replace_placeholder(item, needle, replacement);
            }
        }
        _ => {}
    }
}

const DSH_SKILL: &str = include_str!("../assets/skills/telex/SKILL.md");

fn connect_skill(args: &ConnectArgs) -> Result<()> {
    if !args.write {
        print!("{DSH_SKILL}");
        return Ok(());
    }
    let target = args.file.clone().unwrap_or_else(|| {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
            .join(".dsh")
            .join("skills")
            .join("telex")
            .join("SKILL.md")
    });
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existing = std::fs::read_to_string(&target).unwrap_or_default();
    if existing == DSH_SKILL {
        println!("{} is up to date", target.display());
        return Ok(());
    }
    if !existing.is_empty() {
        std::fs::write(format!("{}.bak", target.display()), &existing).ok();
    }
    std::fs::write(&target, DSH_SKILL)?;
    println!("connected dsh: {}", target.display());
    println!("重启 DSH 会话后生效。");
    Ok(())
}

fn agents_md(args: AgentsMdArgs) -> Result<()> {
    let block = include_str!("../assets/agents-md/telex.md").trim_end();
    let file = &args.file;
    if !args.write {
        println!("{block}");
        return Ok(());
    }
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let next = if let (Some(start), Some(end)) = (
        text.find("<!-- telex:begin -->"),
        text.find("<!-- telex:end -->"),
    ) {
        format!(
            "{}{}{}",
            &text[..start],
            block,
            &text[end + "<!-- telex:end -->".len()..]
        )
    } else if text.trim().is_empty() {
        format!("{block}\n")
    } else {
        format!("{}\n\n{block}\n", text.trim_end())
    };
    if next != text {
        if !text.is_empty() {
            std::fs::write(format!("{}.bak", file.display()), &text).ok();
        }
        std::fs::write(file, next)?;
        println!("updated {}", file.display());
    } else {
        println!("{} is up to date", file.display());
    }
    Ok(())
}

fn inbox_line(home: &Paths, args: InboxLineArgs) -> Result<()> {
    let value = rpc(home, "mail.unread", json!({}))?;
    let count = value["count"].as_u64().unwrap_or(0);
    let role = value["role"].as_str().unwrap_or("");
    if args.json {
        println!("{}", json!({"count": count, "role": role}));
    } else if count == 0 {
        println!("[Telex] {role} 没有未读。");
    } else {
        println!("[Telex] 你有 {count} 封未读（{role}）。查看：`tlx inbox`。");
    }
    Ok(())
}

fn inbox_file(home: &Paths, args: InboxFileArgs) -> Result<()> {
    let value = rpc(home, "mail.list", json!({"folder": "new"}))?;
    let messages = value["messages"].as_array().cloned().unwrap_or_default();
    let role = current_role(home)?.to_string();
    let mut out = format!(
        "# Telex 收件箱（{role}）\n\n由 `tlx` 生成。未读：{}。\n\n",
        messages
            .iter()
            .filter(|entry| entry["unread"].as_bool().unwrap_or(false))
            .count()
    );
    if messages.is_empty() {
        out.push_str("没有邮件。\n");
    } else {
        out.push_str("| 时间 | 发件人 | 主题 | 类型 | id |\n|---|---|---|---|---|\n");
        for entry in messages {
            let subject = entry["subject"].as_str().unwrap_or("").replace('|', "\\|");
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                entry["created_at"].as_str().unwrap_or(""),
                entry["from"].as_str().unwrap_or(""),
                subject,
                entry["intent"].as_str().unwrap_or(""),
                entry["message_id"].as_str().unwrap_or("")
            ));
        }
    }
    std::fs::write(&args.out, out)?;
    println!("wrote {}", args.out.display());
    Ok(())
}

fn doctor(home: &Paths, args: DoctorArgs) -> Result<()> {
    let value = if args.password_stdin {
        let password = read_password(true)?;
        rpc_admin(home, "doctor", json!({}), &password)?
    } else {
        rpc(home, "doctor", json!({}))?
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        let issues = value["issues"].as_array().cloned().unwrap_or_default();
        if issues.is_empty() {
            println!("ok");
        }
        for issue in issues {
            println!(
                "{}: {} ({})",
                issue["code"].as_str().unwrap_or(""),
                issue["message"].as_str().unwrap_or(""),
                issue["path"].as_str().unwrap_or("")
            );
        }
    }
    Ok(())
}

fn index(home: &Paths, cmd: IndexCmd) -> Result<()> {
    match cmd.command {
        IndexSub::Rebuild => {
            let password = read_password(false)?;
            let value = rpc_admin(home, "index.rebuild", json!({}), &password)?;
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
    }
    Ok(())
}
