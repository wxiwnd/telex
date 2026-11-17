//! Local daemon: auth, roles, mail operations and wake scheduling.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::domain::{Address, Envelope, WakePolicy};
use crate::protocol::{ApiError, PROTOCOL_VERSION, Request, Response};
use crate::store::{DeliveryReport, Role, Store};

const WAKE_TIMEOUT_SECS: u64 = 30;

pub fn run(home: PathBuf) -> Result<()> {
    let store = Store::new(home);
    if !store.config_path().exists() {
        bail!("not initialized: run `tlx init` first");
    }
    store.load_config().context("read config.json")?;
    let run_dir = store.run_dir();
    std::fs::create_dir_all(&run_dir)?;

    let socket_path = store.socket_path();
    if UnixStream::connect(&socket_path).is_ok() {
        bail!("another maild is already serving this TELEX_HOME");
    }
    if socket_path.exists() {
        std::fs::remove_file(&socket_path).ok();
    }
    let listener = UnixListener::bind(&socket_path).context("bind maild socket")?;
    std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600)).ok();

    let store = Arc::new(Mutex::new(store));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let store = Arc::clone(&store);
        thread::spawn(move || {
            let _ = handle(store, stream);
        });
    }
    Ok(())
}

fn handle(store: Arc<Mutex<Store>>, stream: UnixStream) -> Result<()> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let mut reader = BufReader::new(stream.try_clone()?).take(16 * 1024 * 1024);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if !line.ends_with('\n') {
        let response = Response::err(0, ApiError::new("protocol", "request is too large"));
        write_response(stream, &response)?;
        return Ok(());
    }
    let request: Request = match serde_json::from_str(line.trim()) {
        Ok(request) => request,
        Err(err) => {
            let response = Response::err(0, ApiError::new("protocol", err.to_string()));
            write_response(stream, &response)?;
            return Ok(());
        }
    };
    let response = match dispatch(&store, &request) {
        Ok(result) => Response::ok(request.id, result),
        Err(error) => Response::err(request.id, error),
    };
    write_response(stream, &response)
}

fn write_response(mut stream: UnixStream, response: &Response) -> Result<()> {
    let mut line = serde_json::to_string(response)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    Ok(())
}

fn dispatch(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    match request.method.as_str() {
        "hello" => Ok(json!({"version": crate::VERSION, "protocol": PROTOCOL_VERSION})),
        "login" => login(store, request),
        "approve" => approve(store, request),
        "assign" => assign(store, request),
        "requests" => requests(store, request),
        "role.add" => role_add(store, request),
        "role.list" => role_list(store, request),
        "role.delete" => role_delete(store, request),
        "admin.passwd" => admin_passwd(store, request),
        "whoami" => whoami(store, request),
        "logout" => logout(store, request),
        "mail.send" => mail_send(store, request),
        "admin.send" => admin_send(store, request),
        "mail.list" => mail_list(store, request),
        "mail.read" => mail_read(store, request),
        "mail.ack" => mail_ack(store, request),
        "mail.claim" => mail_claim(store, request),
        "mail.release" => mail_release(store, request),
        "mail.unread" => mail_unread(store, request),
        "mail.thread" => mail_thread(store, request),
        "wake" => wake(store, request),
        "doctor" => doctor(store, request),
        "index.rebuild" => index_rebuild(store, request),
        other => Err(ApiError::new(
            "not_found",
            format!("unknown method: {other}"),
        )),
    }
}

fn guard<T>(result: anyhow::Result<T>) -> Result<T, ApiError> {
    result.map_err(|err| ApiError::new("validation", err.to_string()))
}

fn param<'a>(params: &'a Value, key: &str) -> Option<&'a Value> {
    params.get(key)
}

fn param_str(params: &Value, key: &str) -> Result<String, ApiError> {
    param(params, key)
        .and_then(Value::as_str)
        .map(|value| value.to_string())
        .ok_or_else(|| ApiError::new("validation", format!("missing parameter: {key}")))
}

fn param_bool(params: &Value, key: &str, default: bool) -> bool {
    param(params, key)
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

fn require_admin(store: &Store, request: &Request) -> Result<(), ApiError> {
    let password = param_str(&request.params, "password")?;
    if store.verify_admin(&password) {
        Ok(())
    } else {
        Err(ApiError::new("not_logged_in", "admin password rejected"))
    }
}

fn require_auth(store: &Store, request: &Request) -> Result<crate::store::Auth, ApiError> {
    let session_id = param_str(&request.params, "session_id")?;
    if session_id.trim().is_empty() {
        return Err(ApiError::new("not_logged_in", "missing session id"));
    }
    let Some(token) = request.token.as_deref() else {
        return Err(revoked_or_missing(store, &session_id));
    };
    let auth = store
        .verify_token(&session_id, token)
        .map_err(|err| ApiError::new("validation", err.to_string()))?
        .ok_or_else(|| revoked_or_missing(store, &session_id))?;
    if store
        .get_role(&auth.role)
        .map_err(|err| ApiError::new("validation", err.to_string()))?
        .is_none()
    {
        return Err(ApiError::new(
            "role_not_found",
            format!("role no longer exists: {}", auth.role),
        ));
    }
    Ok(auth)
}

fn revoked_or_missing(store: &Store, session_id: &str) -> ApiError {
    if store.was_revoked(session_id) {
        ApiError::new("session_revoked", "session is no longer bound to a role")
    } else {
        ApiError::new("not_logged_in", "token invalid or expired")
    }
}

fn login(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let store = store.lock().unwrap();
    let raw = param_str(&request.params, "role")?;
    let domain = store.config().domain;
    let role = guard(Address::parse(&raw, &domain).map_err(anyhow::Error::from))?.to_string();
    let session_id = param_str(&request.params, "session_id")?;
    if session_id.trim().is_empty() {
        return Err(ApiError::new("validation", "session_id is required"));
    }
    let harness = param(&request.params, "harness")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let pending = guard(store.create_pending(&role, &session_id, harness))?;
    Ok(json!({
        "code": pending.code,
        "role": pending.role,
        "expires_at": pending.expires_at,
    }))
}

fn approve(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let store = store.lock().unwrap();
    require_admin(&store, request)?;
    let code = param_str(&request.params, "code")?;
    let pending = guard(store.take_pending(&code))?;
    let auth = guard(store.bind_session(&pending.role, &pending.session_id, &pending.harness))?;
    Ok(json!({
        "role": auth.role,
        "session_id": auth.session_id,
        "expires_at": auth.expires_at,
    }))
}

fn assign(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let store = store.lock().unwrap();
    require_admin(&store, request)?;
    let raw = param_str(&request.params, "role")?;
    let domain = store.config().domain;
    let role = guard(Address::parse(&raw, &domain).map_err(anyhow::Error::from))?.to_string();
    let session_id = param_str(&request.params, "session_id")?;
    if session_id.trim().is_empty() {
        return Err(ApiError::new("validation", "session_id is required"));
    }
    let harness = param(&request.params, "harness")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let auth = guard(store.bind_session(&role, &session_id, harness))?;
    Ok(json!({"role": auth.role, "session_id": auth.session_id, "expires_at": auth.expires_at}))
}

fn requests(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let store = store.lock().unwrap();
    require_admin(&store, request)?;
    let pending = guard(store.list_pending())?;
    Ok(json!({"requests": pending}))
}

fn role_add(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let store = store.lock().unwrap();
    require_admin(&store, request)?;
    let raw = param_str(&request.params, "address")?;
    let domain = store.config().domain;
    let address = guard(Address::parse(&raw, &domain).map_err(anyhow::Error::from))?;
    let display = param(&request.params, "display")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    if let Some(display) = &display {
        if display.chars().count() > 64
            || display
                .chars()
                .any(|c| c == '\r' || c == '\n' || c.is_control())
        {
            return Err(ApiError::new(
                "validation",
                "display must be at most 64 characters without control characters",
            ));
        }
    }
    let wake_cmd = match param(&request.params, "wake_cmd") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(|s| s.to_string())
            .collect(),
        Some(_) => {
            return Err(ApiError::new(
                "validation",
                "wake_cmd must be a JSON array of strings",
            ));
        }
    };
    let role = guard(store.add_role(&address, display, wake_cmd))?;
    Ok(json!({"role": role}))
}

fn role_list(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let store = store.lock().unwrap();
    let has_password = request.params.get("password").is_some();
    let auth = require_auth(&store, request).ok();
    let is_admin = if has_password {
        require_admin(&store, request).is_ok()
    } else {
        false
    };
    if has_password && !is_admin {
        return Err(ApiError::new("not_logged_in", "admin password rejected"));
    }
    if auth.is_none() && !is_admin {
        return Err(ApiError::new(
            "not_logged_in",
            "token or admin password required",
        ));
    }
    let roles = guard(store.list_roles())?;
    let roles: Vec<Value> = roles
        .iter()
        .map(|role| {
            if is_admin {
                json!(role)
            } else {
                json!({"address": role.address, "display": role.display})
            }
        })
        .collect();
    Ok(json!({"roles": roles}))
}

fn role_delete(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let store = store.lock().unwrap();
    require_admin(&store, request)?;
    let role = param_str(&request.params, "role")?;
    let domain = store.config().domain;
    let role = guard(Address::parse(&role, &domain).map_err(anyhow::Error::from))?.to_string();
    guard(store.revoke_role_sessions(&role))?;
    guard(store.clear_claims_for(&role))?;
    guard(store.remove_pending_for_role(&role))?;
    store.clear_role_runtime(&role);
    let path = store.roles_dir().join(format!("{role}.json"));
    std::fs::remove_file(path).ok();
    Ok(json!({"ok": true}))
}

fn admin_passwd(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let store = store.lock().unwrap();
    require_admin(&store, request)?;
    let new_password = param_str(&request.params, "new_password")?;
    if new_password.chars().count() < 12 {
        return Err(ApiError::new(
            "validation",
            "password must be at least 12 characters",
        ));
    }
    guard(store.set_admin_password(&new_password))?;
    guard(store.revoke_all_sessions())?;
    Ok(json!({"ok": true}))
}

fn whoami(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let store = store.lock().unwrap();
    let auth = require_auth(&store, request)?;
    Ok(json!({"role": auth.role, "session_id": auth.session_id, "harness": auth.harness}))
}

fn logout(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let store = store.lock().unwrap();
    let auth = require_auth(&store, request)?;
    guard(store.revoke_session(&auth.session_id))?;
    Ok(json!({"ok": true}))
}

fn mail_send(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let store_arc = Arc::clone(store);
    let store = store.lock().unwrap();
    let auth = require_auth(&store, request)?;
    let role = guard(store.get_role(&auth.role))?
        .ok_or_else(|| ApiError::new("role_not_found", "role missing"))?;
    let envelope = parse_envelope(request)?;
    let sent = send_envelope(&store, envelope, &role)?;
    drop(store);
    finish_send(&store_arc, &sent);
    Ok(send_result_json(&sent))
}

fn admin_send(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let store_arc = Arc::clone(store);
    let store = store.lock().unwrap();
    require_admin(&store, request)?;
    let from_raw = param_str(&request.params, "from")?;
    let domain = store.config().domain;
    let from = guard(Address::parse(&from_raw, &domain).map_err(anyhow::Error::from))?;
    let role = guard(store.get_role(from.as_str()))?
        .ok_or_else(|| ApiError::new("role_not_found", format!("role not found: {from}")))?;
    let envelope = parse_envelope(request)?;
    let sent = send_envelope(&store, envelope, &role)?;
    drop(store);
    finish_send(&store_arc, &sent);
    Ok(send_result_json(&sent))
}

struct Sent {
    report: DeliveryReport,
    wake_targets: Vec<String>,
    envelope: Envelope,
}

fn parse_envelope(request: &Request) -> Result<Envelope, ApiError> {
    serde_json::from_value(
        param(&request.params, "envelope")
            .cloned()
            .ok_or_else(|| ApiError::new("validation", "missing envelope"))?,
    )
    .map_err(|err| ApiError::new("validation", err.to_string()))
}

fn send_envelope(store: &Store, mut envelope: Envelope, role: &Role) -> Result<Sent, ApiError> {
    envelope.from.address =
        guard(Address::parse(&role.address, "local").map_err(anyhow::Error::from))?;
    envelope.from.display = role.display.clone();
    envelope.dedupe_key = envelope.compute_dedupe_key();

    let parent = match envelope.in_reply_to {
        Some(parent_id) => match store.read_message(&role.address, &parent_id.to_string(), false) {
            Ok(message) => Some(message.envelope),
            Err(_) => {
                return Err(ApiError::new(
                    "validation",
                    format!("parent message not found: {parent_id}"),
                ));
            }
        },
        None => None,
    };
    let errors = envelope.validate(parent.as_ref());
    if !errors.is_empty() {
        return Err(ApiError::new("validation", error_text(&errors)));
    }

    let mut recipients = Vec::new();
    for address in envelope
        .to
        .iter()
        .chain(envelope.cc.iter())
        .chain(envelope.bcc.iter())
    {
        if guard(store.get_role(address.as_str()))?.is_none() {
            return Err(ApiError::new(
                "role_not_found",
                format!("role not found: {address}"),
            ));
        }
        recipients.push(address.to_string());
    }

    let report = guard(store.deliver(&envelope, &recipients))?;
    let wake_targets: Vec<_> = envelope
        .to
        .iter()
        .chain(envelope.bcc.iter())
        .map(|address| address.to_string())
        .filter(|address| report.delivered.contains(address))
        .collect();
    Ok(Sent {
        report,
        wake_targets,
        envelope,
    })
}

fn error_text(errors: &[crate::domain::ValidationError]) -> String {
    errors
        .iter()
        .map(|error| error.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

fn finish_send(store: &Arc<Mutex<Store>>, sent: &Sent) {
    for address in &sent.wake_targets {
        maybe_wake(store, address.as_str(), &sent.envelope);
    }
}

fn send_result_json(sent: &Sent) -> Value {
    json!({
        "delivered": sent.report.delivered,
        "duplicates": sent.report.duplicates,
        "message_id": sent.envelope.message_id.to_string(),
    })
}

fn maybe_wake(store: &Arc<Mutex<Store>>, role: &str, envelope: &Envelope) {
    if envelope.wake_policy == WakePolicy::Never {
        return;
    }
    if envelope
        .expires_at
        .is_some_and(|expires| expires <= chrono::Utc::now())
    {
        return;
    }
    let store = Arc::clone(store);
    let role = role.to_string();
    let message_id = envelope.message_id.to_string();
    thread::spawn(move || wake_loop(store, role, message_id));
}

fn wake_loop(store: Arc<Mutex<Store>>, role: String, message_id: String) {
    let (cmd, session, harness, count) = {
        let store = store.lock().unwrap();
        let Ok(Some(role_entry)) = store.get_role(&role) else {
            store.clear_wake_failure(&role);
            return;
        };
        let count = store.unread_count(&role).unwrap_or(0);
        (
            role_entry.wake_cmd.clone(),
            role_entry.session_id.clone(),
            role_entry.harness.clone(),
            count,
        )
    };
    let Some(session) = session else {
        let store = store.lock().unwrap();
        store.log_wake(&role, "skipped: role has no bound session");
        return;
    };
    if cmd.is_empty() {
        let store = store.lock().unwrap();
        store.clear_wake_failure(&role);
        return;
    }

    let seq = {
        let store = store.lock().unwrap();
        let mut state = store.wake_state(&role);
        state.seq += 1;
        state.last_at = Some(
            chrono::Utc::now()
                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                .to_string(),
        );
        let seq = state.seq;
        let _ = store.save_wake_state(&role, &state);
        seq
    };

    let delays = [0u64, 10, 60, 120];
    let mut last_error = String::new();
    for (attempt, delay) in delays.iter().enumerate() {
        if *delay > 0 {
            thread::sleep(Duration::from_secs(*delay));
        }
        let vars = [
            ("address", role.clone()),
            ("count", count.to_string()),
            ("session_id", session.clone()),
            ("harness", harness.clone().unwrap_or_default()),
            ("seq", seq.to_string()),
        ];
        let args: Vec<String> = cmd.iter().map(|arg| substitute(arg, &vars)).collect();
        let status = run_wake_command(&args, Duration::from_secs(WAKE_TIMEOUT_SECS));
        let (ok, line, error) = match status {
            Ok(Some(status)) if status.success() => (
                true,
                format!("attempt {} ok: {}", attempt + 1, args.join(" ")),
                String::new(),
            ),
            Ok(Some(status)) => (
                false,
                format!(
                    "attempt {} failed with {:?}: {}",
                    attempt + 1,
                    status.code(),
                    args.join(" ")
                ),
                format!("exit code {:?}", status.code()),
            ),
            Ok(None) => (
                false,
                format!(
                    "attempt {} timed out after {}s: {}",
                    attempt + 1,
                    WAKE_TIMEOUT_SECS,
                    args.join(" ")
                ),
                format!("timed out after {WAKE_TIMEOUT_SECS}s"),
            ),
            Err(err) => (
                false,
                format!("attempt {} spawn error: {err}", attempt + 1),
                format!("spawn error: {err}"),
            ),
        };
        {
            let store = store.lock().unwrap();
            store.log_wake(&role, &line);
        }
        last_error = error;
        if ok {
            let store = store.lock().unwrap();
            store.clear_wake_failure(&role);
            return;
        }
    }
    let store = store.lock().unwrap();
    store.log_wake(&role, "wake_failed");
    let _ = store.record_wake_failure(&role, &message_id, delays.len() as u32, &last_error);
}

fn run_wake_command(
    args: &[String],
    timeout: Duration,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    let mut child = std::process::Command::new(&args[0])
        .args(&args[1..])
        .spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn substitute(template: &str, vars: &[(&str, String)]) -> String {
    let mut out = template.to_string();
    for (key, value) in vars {
        out = out.replace(&format!("{{{key}}}"), value);
    }
    out
}

fn mail_list(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let store = store.lock().unwrap();
    let auth = require_auth(&store, request)?;
    let folder = param(&request.params, "folder")
        .and_then(Value::as_str)
        .unwrap_or("new");
    let entries = guard(store.list(&auth.role, folder))?;
    Ok(json!({"messages": entries}))
}

fn mail_read(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let store = store.lock().unwrap();
    let auth = require_auth(&store, request)?;
    let message_id = param_str(&request.params, "message_id")?;
    let mark_seen = param_bool(&request.params, "mark_seen", true);
    let message = guard(store.read_message(&auth.role, &message_id, mark_seen))?;
    Ok(json!({
        "envelope": message.envelope,
        "body": message.body,
        "seen": message.seen,
        "acked": message.acked,
        "folder": message.folder,
        "path": message.path,
    }))
}

fn mail_ack(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let store = store.lock().unwrap();
    let auth = require_auth(&store, request)?;
    let message_id = param_str(&request.params, "message_id")?;
    guard(store.ack(&auth.role, &message_id))?;
    Ok(json!({"ok": true}))
}

fn mail_claim(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let store = store.lock().unwrap();
    let auth = require_auth(&store, request)?;
    let message_id = param_str(&request.params, "message_id")?;
    let claimed = guard(store.claim(&auth.role, &message_id))?;
    Ok(json!({"claimed": claimed, "owner": auth.role}))
}

fn mail_release(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let store = store.lock().unwrap();
    let auth = require_auth(&store, request)?;
    let message_id = param_str(&request.params, "message_id")?;
    let released = guard(store.release(&auth.role, &message_id))?;
    Ok(json!({"released": released}))
}

fn mail_unread(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let store = store.lock().unwrap();
    let auth = require_auth(&store, request)?;
    let count = guard(store.unread_count(&auth.role))?;
    Ok(json!({"count": count, "role": auth.role}))
}

fn mail_thread(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let store = store.lock().unwrap();
    let auth = require_auth(&store, request)?;
    let id = param_str(&request.params, "thread_id")?;
    let messages = guard(store.thread(&auth.role, &id))?;
    Ok(json!({"messages": messages}))
}

fn wake(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let raw = param_str(&request.params, "role")?;
    let role = {
        let store = store.lock().unwrap();
        let domain = store.config().domain;
        let role = guard(Address::parse(&raw, &domain).map_err(anyhow::Error::from))?.to_string();
        if guard(store.get_role(&role))?.is_none() {
            return Err(ApiError::new(
                "role_not_found",
                format!("role not found: {role}"),
            ));
        }
        let auth = require_auth(&store, request);
        let own_role = matches!(&auth, Ok(auth) if auth.role == role);
        if !own_role {
            if request.params.get("password").is_some() {
                require_admin(&store, request)?;
            } else {
                return Err(match auth {
                    Err(err) => err,
                    Ok(_) => ApiError::new("not_logged_in", "can only wake your own role"),
                });
            }
        }
        role
    };
    let normalized = role.clone();
    let store = Arc::clone(store);
    thread::spawn(move || wake_loop(store, role, String::new()));
    Ok(json!({"ok": true, "role": normalized}))
}

fn doctor(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let store = store.lock().unwrap();
    if require_auth(&store, request).is_err() {
        require_admin(&store, request)?;
    }
    let issues = guard(store.doctor())?;
    Ok(json!({"issues": issues}))
}

fn index_rebuild(store: &Arc<Mutex<Store>>, request: &Request) -> Result<Value, ApiError> {
    let store = store.lock().unwrap();
    require_admin(&store, request)?;
    let counts = guard(store.index_rebuild())?;
    Ok(counts)
}
