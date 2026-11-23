//! File-backed spool plus roles, bindings, tokens and pending requests.
//!
//! The daemon is the only writer. Tests call `Store` directly.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::domain::canonical::sha256_hex;
use crate::domain::{Address, Envelope};
use crate::index::{IndexedMessage, MailIndex};
use crate::paths::Paths;
use crate::process::ancestor_pids;

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

pub const TOKEN_TTL_HOURS: i64 = 12;
pub const PENDING_TTL_MINUTES: i64 = 5;
const TMP_MAX_AGE_HOURS: i64 = 36;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub domain: String,
    #[serde(default)]
    pub admin_password: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            domain: "local".to_string(),
            admin_password: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Role {
    pub address: String,
    #[serde(default)]
    pub display: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub harness: Option<String>,
    #[serde(default)]
    pub bound_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pending {
    pub code: String,
    pub role: String,
    pub session_id: String,
    pub harness: String,
    pub created_at: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Auth {
    pub role: String,
    pub session_id: String,
    pub harness: String,
    pub token_hash: String,
    pub created_at: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecipientStatus {
    pub address: String,
    pub delivered: bool,
    pub seen_at: Option<String>,
    pub acked_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MailEntry {
    pub message_id: String,
    pub thread_id: String,
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub created_at: String,
    pub intent: String,
    pub disposition: String,
    pub priority: u8,
    pub unread: bool,
    pub seen: bool,
    pub acked: bool,
    pub expired: bool,
    pub recipients: Vec<RecipientStatus>,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadMessage {
    pub envelope: Envelope,
    pub body: String,
    pub seen: bool,
    pub acked: bool,
    pub path: String,
    pub folder: String,
}

#[derive(Debug, Default)]
pub struct DeliveryReport {
    pub delivered: Vec<String>,
    pub duplicates: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorIssue {
    pub code: String,
    pub message: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RevokedSession {
    role: String,
    at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionHandoff {
    session_id: String,
    harness: String,
    cwd: String,
    created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteOutcome {
    Written,
    Duplicate,
    Conflict,
}

pub struct Store {
    paths: Paths,
}

impl Store {
    pub fn new(paths: Paths) -> Self {
        Self { paths }
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    pub fn config_path(&self) -> PathBuf {
        self.paths.config_file()
    }

    pub fn index_path(&self) -> PathBuf {
        self.paths.index_file()
    }

    pub fn roles_dir(&self) -> PathBuf {
        self.paths.seats_dir()
    }

    pub fn init(&self, admin_password: &str, domain: &str) -> Result<()> {
        if admin_password.chars().count() < 12 {
            bail!("admin password must be at least 12 characters");
        }
        let domain = domain.to_ascii_lowercase();
        Address::parse("probe", &domain).map_err(|err| anyhow!("invalid domain: {err}"))?;
        for dir in [
            self.paths.config_dir().to_path_buf(),
            self.paths.data_dir().to_path_buf(),
            self.paths.state_dir().to_path_buf(),
            self.paths.cache_dir().to_path_buf(),
            self.paths.runtime_dir().to_path_buf(),
            self.paths.mail_dir(),
            self.paths.markers_dir(),
            self.paths.claims_dir(),
            self.paths.seats_dir(),
            self.paths.pending_dir(),
            self.paths.auth_dir(),
            self.paths.tokens_dir(),
            self.paths.revoked_dir(),
            self.paths.hook_dir(),
        ] {
            fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        }
        for dir in [
            self.paths.config_dir(),
            self.paths.data_dir(),
            self.paths.state_dir(),
            self.paths.cache_dir(),
            self.paths.runtime_dir(),
        ] {
            let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        }
        MailIndex::create(&self.index_path())?;
        let config = Config {
            domain,
            admin_password: Some(hash_password(admin_password)?),
        };
        write_json_atomic(&self.config_path(), &config)?;
        Ok(())
    }

    pub fn load_config(&self) -> Result<Config> {
        let config: Config = read_json(&self.config_path())?;
        Address::parse("probe", &config.domain)
            .map_err(|err| anyhow!("invalid domain in config.json: {err}"))?;
        Ok(config)
    }

    pub fn config(&self) -> Config {
        self.load_config().unwrap_or_default()
    }

    pub fn save_config(&self, config: &Config) -> Result<()> {
        write_json_atomic(&self.config_path(), config)
    }

    pub fn set_admin_password(&self, password: &str) -> Result<()> {
        if password.chars().count() < 12 {
            bail!("admin password must be at least 12 characters");
        }
        let mut config = self.config();
        config.admin_password = Some(hash_password(password)?);
        self.save_config(&config)
    }

    pub fn verify_admin(&self, password: &str) -> bool {
        self.config()
            .admin_password
            .as_deref()
            .is_some_and(|stored| verify_password(password, stored))
    }

    // ---------------------------------------------------------------- roles

    fn role_path(&self, address: &str) -> PathBuf {
        let safe: String = address
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@'))
            .collect();
        self.roles_dir().join(format!("{safe}.json"))
    }

    pub fn get_role(&self, address: &str) -> Result<Option<Role>> {
        let path = self.role_path(address);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(read_json(&path)?))
    }

    pub fn list_roles(&self) -> Result<Vec<Role>> {
        let mut roles = Vec::new();
        for entry in fs::read_dir(self.roles_dir()).context("read seats dir")? {
            let entry = entry?;
            if entry.path().extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(role) = read_json::<Role>(&entry.path()) {
                roles.push(role);
            }
        }
        roles.sort_by(|a, b| a.address.cmp(&b.address));
        Ok(roles)
    }

    pub fn add_role(&self, address: &Address, display: Option<String>) -> Result<Role> {
        if self.get_role(address.as_str())?.is_some() {
            bail!("role already exists: {address}");
        }
        let role = Role {
            address: address.to_string(),
            display,
            session_id: None,
            harness: None,
            bound_at: None,
            created_at: now_rfc3339(),
        };
        write_json_atomic(&self.role_path(address.as_str()), &role)?;
        Ok(role)
    }

    pub fn save_role(&self, role: &Role) -> Result<()> {
        write_json_atomic(&self.role_path(&role.address), role)
    }

    // ---------------------------------------------------------------- pending

    fn pending_dir(&self) -> PathBuf {
        self.paths.pending_dir()
    }

    pub fn create_pending(&self, role: &str, session_id: &str, harness: &str) -> Result<Pending> {
        if self.get_role(role)?.is_none() {
            bail!("role not found: {role}");
        }
        let code = loop {
            let code = format!("{:06}", rand::random::<u32>() % 1_000_000);
            if !self.pending_dir().join(format!("{code}.json")).exists() {
                break code;
            }
        };
        let now = Utc::now();
        let pending = Pending {
            code: code.clone(),
            role: role.to_string(),
            session_id: session_id.to_string(),
            harness: harness.to_string(),
            created_at: rfc3339(now),
            expires_at: rfc3339(now + Duration::minutes(PENDING_TTL_MINUTES)),
        };
        write_json_atomic(&self.pending_dir().join(format!("{code}.json")), &pending)?;
        Ok(pending)
    }

    pub fn take_pending(&self, code: &str) -> Result<Pending> {
        let path = self.pending_dir().join(format!("{code}.json"));
        let pending: Pending =
            read_json(&path).with_context(|| format!("pending code not found: {code}"))?;
        let expires = parse_rfc3339(&pending.expires_at)?;
        fs::remove_file(&path).ok();
        if expires < Utc::now() {
            bail!("pending code expired");
        }
        Ok(pending)
    }

    pub fn list_pending(&self) -> Result<Vec<Pending>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(self.pending_dir()).context("read pending dir")? {
            let entry = entry?;
            if let Ok(pending) = read_json::<Pending>(&entry.path()) {
                if parse_rfc3339(&pending.expires_at).is_ok_and(|expires| expires > Utc::now()) {
                    out.push(pending);
                } else {
                    fs::remove_file(entry.path()).ok();
                }
            }
        }
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(out)
    }

    // ---------------------------------------------------------------- auth

    fn auth_path(&self, session_id: &str) -> PathBuf {
        self.paths
            .auth_dir()
            .join(format!("{}.json", session_key(session_id)))
    }

    pub fn token_path(&self, session_id: &str) -> PathBuf {
        self.paths
            .tokens_dir()
            .join(format!("{}.tok", session_key(session_id)))
    }

    fn revoked_path(&self, session_id: &str) -> PathBuf {
        self.paths
            .revoked_dir()
            .join(format!("{}.json", session_key(session_id)))
    }

    fn auth_for_session(&self, session_id: &str) -> Result<Option<Auth>> {
        let path = self.auth_path(session_id);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(read_json(&path)?))
    }

    pub fn verify_token(&self, session_id: &str, token: &str) -> Result<Option<Auth>> {
        let Some(auth) = self.auth_for_session(session_id)? else {
            return Ok(None);
        };
        if parse_rfc3339(&auth.expires_at)? < Utc::now() {
            return Ok(None);
        }
        if auth.token_hash != token_hash(token) {
            return Ok(None);
        }
        Ok(Some(auth))
    }

    pub fn was_revoked(&self, session_id: &str) -> bool {
        self.revoked_path(session_id).exists()
    }

    pub fn issue_token(&self, role: &str, session_id: &str, harness: &str) -> Result<Auth> {
        let token = random_token();
        let now = Utc::now();
        let auth = Auth {
            role: role.to_string(),
            session_id: session_id.to_string(),
            harness: harness.to_string(),
            token_hash: token_hash(&token),
            created_at: rfc3339(now),
            expires_at: rfc3339(now + Duration::hours(TOKEN_TTL_HOURS)),
        };
        write_json_atomic(&self.auth_path(session_id), &auth)?;
        write_private(&self.token_path(session_id), token.as_bytes())?;
        fs::remove_file(self.revoked_path(session_id)).ok();
        Ok(auth)
    }

    pub fn read_token_file(&self, session_id: &str) -> Option<String> {
        fs::read_to_string(self.token_path(session_id))
            .ok()
            .map(|s| s.trim().to_string())
    }

    pub fn revoke_session(&self, session_id: &str) -> Result<()> {
        let auth_path = self.auth_path(session_id);
        let token_path = self.token_path(session_id);
        let role = read_json::<Auth>(&auth_path).ok().map(|auth| auth.role);
        if let Some(role_addr) = &role {
            let revoked = RevokedSession {
                role: role_addr.clone(),
                at: now_rfc3339(),
            };
            let path = self.revoked_path(session_id);
            write_json_atomic(&path, &revoked)?;
        }
        fs::remove_file(&auth_path).ok();
        fs::remove_file(&token_path).ok();
        if let Some(role_addr) = role {
            if let Some(mut role) = self.get_role(&role_addr)? {
                if role.session_id.as_deref() == Some(session_id) {
                    role.session_id = None;
                    role.harness = None;
                    role.bound_at = None;
                    self.save_role(&role)?;
                }
            }
        }
        Ok(())
    }

    pub fn revoke_role_sessions(&self, role_addr: &str) -> Result<()> {
        let mut sessions = Vec::new();
        if let Ok(entries) = fs::read_dir(self.paths.auth_dir()) {
            for entry in entries.flatten() {
                if let Ok(auth) = read_json::<Auth>(&entry.path()) {
                    if auth.role == role_addr {
                        sessions.push(auth.session_id);
                    }
                }
            }
        }
        for session in sessions {
            self.revoke_session(&session)?;
        }
        Ok(())
    }

    pub fn revoke_all_sessions(&self) -> Result<()> {
        let mut sessions = Vec::new();
        if let Ok(entries) = fs::read_dir(self.paths.auth_dir()) {
            for entry in entries.flatten() {
                if let Ok(auth) = read_json::<Auth>(&entry.path()) {
                    sessions.push(auth.session_id);
                }
            }
        }
        for session in sessions {
            self.revoke_session(&session)?;
        }
        Ok(())
    }

    pub fn bind_session(&self, role_addr: &str, session_id: &str, harness: &str) -> Result<Auth> {
        let mut role = self
            .get_role(role_addr)?
            .ok_or_else(|| anyhow!("role not found: {role_addr}"))?;
        if let Some(existing) = self.auth_for_session(session_id)? {
            if existing.role != role_addr {
                self.revoke_session(session_id)?;
            }
        }
        if let Some(old_session) = role.session_id.clone() {
            if old_session != session_id {
                self.revoke_session(&old_session)?;
            }
        }
        self.clear_claims_for(role_addr)?;
        let auth = self.issue_token(role_addr, session_id, harness)?;
        role.session_id = Some(session_id.to_string());
        role.harness = Some(harness.to_string());
        role.bound_at = Some(now_rfc3339());
        self.save_role(&role)?;
        Ok(auth)
    }

    pub fn role_by_session(&self, session_id: &str) -> Result<Option<Role>> {
        let Some(auth) = self.auth_for_session(session_id)? else {
            return Ok(None);
        };
        self.get_role(&auth.role)
    }

    pub fn record_session_handoff(
        &self,
        session_id: &str,
        harness: &str,
        cwd: &Path,
    ) -> Result<()> {
        let dir = self.paths.session_handoff_dir();
        fs::create_dir_all(&dir)?;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
        let handoff = SessionHandoff {
            session_id: session_id.to_string(),
            harness: harness.to_string(),
            cwd: cwd.display().to_string(),
            created_at: now_rfc3339(),
        };
        for pid in ancestor_pids() {
            write_json_atomic(&dir.join(format!("{pid}.json")), &handoff)?;
        }
        Ok(())
    }

    pub fn find_session_handoff(&self) -> Option<String> {
        let dir = self.paths.session_handoff_dir();
        for pid in ancestor_pids() {
            let path = dir.join(format!("{pid}.json"));
            let Ok(handoff) = read_json::<SessionHandoff>(&path) else {
                continue;
            };
            let Ok(created_at) = parse_rfc3339(&handoff.created_at) else {
                continue;
            };
            if Utc::now() - created_at < Duration::hours(24) && !handoff.session_id.is_empty() {
                return Some(handoff.session_id);
            }
        }
        None
    }

    // ---------------------------------------------------------------- mail paths

    pub fn mailbox_dir(&self, address: &str) -> PathBuf {
        self.paths.mail_dir().join(address)
    }

    fn folder_dir(&self, address: &str, folder: &str) -> PathBuf {
        self.mailbox_dir(address).join(folder)
    }

    fn message_path(&self, address: &str, folder: &str, message_id: &str) -> PathBuf {
        self.folder_dir(address, folder)
            .join(format!("{message_id}.json"))
    }

    // ---------------------------------------------------------------- delivery

    pub fn deliver(&self, envelope: &Envelope, recipients: &[String]) -> Result<DeliveryReport> {
        let mut report = DeliveryReport::default();
        let mut index = self.open_index();
        for recipient in recipients {
            let mut copy = envelope.clone();
            copy.bcc.clear();
            copy.dedupe_key = copy.compute_dedupe_key();
            let path = self.message_path(recipient, "new", &copy.message_id.to_string());
            match self.write_message(&path, &copy)? {
                WriteOutcome::Written => report.delivered.push(recipient.clone()),
                WriteOutcome::Duplicate => report.duplicates.push(recipient.clone()),
                WriteOutcome::Conflict => bail!("id-conflict: {}", copy.message_id),
            }
            self.index_upsert(
                &mut index,
                &IndexedMessage::from_envelope(&copy, recipient, "new"),
            );
        }
        let sent_path = self.message_path(
            &envelope.from.address.to_string(),
            "sent",
            &envelope.message_id.to_string(),
        );
        if self.write_message(&sent_path, envelope)? == WriteOutcome::Conflict {
            bail!("id-conflict in sent copy: {}", envelope.message_id);
        }
        self.index_upsert(
            &mut index,
            &IndexedMessage::from_envelope(envelope, &envelope.from.address.to_string(), "sent"),
        );
        Ok(report)
    }

    fn open_index(&self) -> Option<MailIndex> {
        let path = self.index_path();
        if !path.exists() {
            return None;
        }
        match MailIndex::open(&path) {
            Ok(index) => Some(index),
            Err(_) => {
                fs::remove_file(&path).ok();
                None
            }
        }
    }

    fn index_upsert(&self, index: &mut Option<MailIndex>, message: &IndexedMessage) {
        let failed = match index.as_ref() {
            Some(active) => active.upsert(message).is_err(),
            None => false,
        };
        if failed {
            *index = None;
            fs::remove_file(self.index_path()).ok();
        }
    }

    fn write_message(&self, target: &Path, envelope: &Envelope) -> Result<WriteOutcome> {
        let bytes = envelope.canonical_bytes();
        if target.exists() {
            let existing = fs::read(target)?;
            if existing == bytes {
                return Ok(WriteOutcome::Duplicate);
            } else {
                return Ok(WriteOutcome::Conflict);
            }
        }
        let folder = target.parent().ok_or_else(|| anyhow!("bad message path"))?;
        fs::create_dir_all(folder)?;
        let _ = fs::set_permissions(folder, fs::Permissions::from_mode(0o700));
        let mailbox = folder.parent().ok_or_else(|| anyhow!("bad mailbox path"))?;
        let tmp_dir = mailbox.join("tmp");
        fs::create_dir_all(&tmp_dir)?;
        let _ = fs::set_permissions(&tmp_dir, fs::Permissions::from_mode(0o700));

        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = tmp_dir.join(format!(
            "{}.{}.{}.json",
            envelope.message_id,
            std::process::id(),
            seq
        ));
        {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)
                .context("create temp message")?;
            file.write_all(&bytes)?;
            file.sync_all().ok();
        }
        match fs::hard_link(&tmp, target) {
            Ok(()) => {
                fs::remove_file(&tmp).ok();
                sync_dir(folder);
                Ok(WriteOutcome::Written)
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                fs::remove_file(&tmp).ok();
                let existing = fs::read(target)?;
                if existing == bytes {
                    Ok(WriteOutcome::Duplicate)
                } else {
                    Ok(WriteOutcome::Conflict)
                }
            }
            Err(err) => {
                fs::remove_file(&tmp).ok();
                Err(err).context("link message into mailbox")
            }
        }
    }

    // ---------------------------------------------------------------- reading

    pub fn list(&self, role: &str, folder: &str) -> Result<Vec<MailEntry>> {
        if folder != "new" && folder != "sent" {
            bail!("invalid folder: {folder}");
        }
        if let Some(index) = self.open_index() {
            match index.list(role, folder) {
                Ok(rows) => {
                    let mut entries = Vec::with_capacity(rows.len());
                    for row in rows {
                        entries.push(self.entry_from_index(&index, role, row));
                    }
                    return Ok(entries);
                }
                Err(_) => {
                    fs::remove_file(self.index_path()).ok();
                }
            }
        }
        self.scan_list(role, folder)
    }

    fn scan_list(&self, role: &str, folder: &str) -> Result<Vec<MailEntry>> {
        let dir = self.folder_dir(role, folder);
        let mut entries = Vec::new();
        let read_dir = match fs::read_dir(&dir) {
            Ok(read_dir) => read_dir,
            Err(_) => return Ok(entries),
        };
        for entry in read_dir {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let meta = match fs::metadata(&path) {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            if meta.len() == 0 {
                continue;
            }
            let envelope: Envelope = match read_json(&path) {
                Ok(envelope) => envelope,
                Err(_) => continue,
            };
            entries.push(self.entry_from(role, &path, &envelope, folder));
        }
        entries.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.message_id.cmp(&a.message_id))
        });
        Ok(entries)
    }

    fn entry_from_index(
        &self,
        index: &MailIndex,
        role: &str,
        message: IndexedMessage,
    ) -> MailEntry {
        let message_id = message.message_id.clone();
        let seen = self.has_marker(&message_id, "seen", role);
        let acked = self.has_marker(&message_id, "ack", role);
        let expired = message
            .expires_at
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|value| value.with_timezone(&Utc) < Utc::now());
        let show_bcc = message.folder == "sent" || message.from_addr == role;
        let recipients = if message.folder == "sent" {
            self.recipient_statuses_indexed(
                index,
                &message_id,
                &message.to,
                &message.cc,
                &message.bcc,
            )
        } else {
            Vec::new()
        };
        let path = self
            .message_path(role, &message.folder, &message_id)
            .display()
            .to_string();
        MailEntry {
            message_id: message_id.clone(),
            thread_id: message.thread_id,
            from: message.from_addr,
            to: message.to,
            cc: message.cc,
            bcc: if show_bcc { message.bcc } else { Vec::new() },
            subject: message.subject,
            created_at: message.created_at,
            intent: message.intent,
            disposition: message.disposition,
            priority: message.priority,
            unread: message.folder == "new" && !seen && !expired,
            seen,
            acked,
            expired,
            recipients,
            path,
        }
    }

    fn recipient_statuses_indexed(
        &self,
        index: &MailIndex,
        message_id: &str,
        to: &[String],
        cc: &[String],
        bcc: &[String],
    ) -> Vec<RecipientStatus> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for address in to.iter().chain(cc.iter()).chain(bcc.iter()) {
            if !seen.insert(address.clone()) {
                continue;
            }
            out.push(RecipientStatus {
                delivered: index.has_copy(address, message_id).unwrap_or(false),
                seen_at: self.marker_time(message_id, "seen", address),
                acked_at: self.marker_time(message_id, "ack", address),
                address: address.clone(),
            });
        }
        out
    }

    fn entry_from(&self, role: &str, path: &Path, envelope: &Envelope, folder: &str) -> MailEntry {
        let message_id = envelope.message_id.to_string();
        let seen = self.has_marker(&message_id, "seen", role);
        let acked = self.has_marker(&message_id, "ack", role);
        let expired = envelope
            .expires_at
            .is_some_and(|expires| expires < Utc::now());
        let show_bcc = folder == "sent" || envelope.from.address.as_str() == role;
        MailEntry {
            message_id: message_id.clone(),
            thread_id: envelope.thread_id.to_string(),
            from: envelope.from.address.to_string(),
            to: envelope.to.iter().map(|a| a.to_string()).collect(),
            cc: envelope.cc.iter().map(|a| a.to_string()).collect(),
            bcc: if show_bcc {
                envelope.bcc.iter().map(|a| a.to_string()).collect()
            } else {
                Vec::new()
            },
            subject: envelope.subject.clone(),
            created_at: rfc3339(envelope.created_at),
            intent: serde_json::to_string(&envelope.intent)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string(),
            disposition: serde_json::to_string(&envelope.disposition)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string(),
            priority: envelope.priority,
            unread: folder == "new" && !seen && !expired,
            seen,
            acked,
            expired,
            recipients: if folder == "sent" {
                self.recipient_statuses(envelope)
            } else {
                Vec::new()
            },
            path: path.display().to_string(),
        }
    }

    fn recipient_statuses(&self, envelope: &Envelope) -> Vec<RecipientStatus> {
        let message_id = envelope.message_id.to_string();
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for address in envelope
            .to
            .iter()
            .chain(envelope.cc.iter())
            .chain(envelope.bcc.iter())
        {
            let address = address.to_string();
            if !seen.insert(address.clone()) {
                continue;
            }
            out.push(RecipientStatus {
                delivered: self.message_path(&address, "new", &message_id).exists(),
                seen_at: self.marker_time(&message_id, "seen", &address),
                acked_at: self.marker_time(&message_id, "ack", &address),
                address,
            });
        }
        out
    }

    pub fn read_message(
        &self,
        role: &str,
        message_id: &str,
        mark_seen: bool,
    ) -> Result<ReadMessage> {
        ensure_message_id(message_id)?;
        let (path, folder) = if self.message_path(role, "new", message_id).exists() {
            (self.message_path(role, "new", message_id), "new")
        } else if self.message_path(role, "sent", message_id).exists() {
            (self.message_path(role, "sent", message_id), "sent")
        } else {
            bail!("message not found: {message_id}");
        };
        let envelope: Envelope = read_json(&path)?;
        let body = envelope
            .body
            .decode_text()
            .map_err(|err| anyhow!("body is not valid text: {err}"))?;
        if mark_seen && folder == "new" {
            self.mark(role, message_id, "seen")?;
        }
        let seen = self.has_marker(message_id, "seen", role);
        let acked = self.has_marker(message_id, "ack", role);
        Ok(ReadMessage {
            body,
            envelope,
            seen,
            acked,
            path: path.display().to_string(),
            folder: folder.to_string(),
        })
    }

    // ---------------------------------------------------------------- state

    fn state_dir(&self, message_id: &str) -> PathBuf {
        self.paths.markers_dir().join(message_id)
    }

    fn marker_times(&self, message_id: &str, kind: &str, role: &str) -> Vec<i64> {
        let dir = self.state_dir(message_id);
        let prefix = format!("{kind}.{role}.");
        let mut out = Vec::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let Some(suffix) = name.strip_prefix(&prefix) else {
                    continue;
                };
                if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                if let Ok(ts) = suffix.parse::<i64>() {
                    out.push(ts);
                }
            }
        }
        out
    }

    pub fn has_marker(&self, message_id: &str, kind: &str, role: &str) -> bool {
        !self.marker_times(message_id, kind, role).is_empty()
    }

    fn marker_time(&self, message_id: &str, kind: &str, role: &str) -> Option<String> {
        self.marker_times(message_id, kind, role)
            .into_iter()
            .min()
            .and_then(DateTime::from_timestamp_millis)
            .map(rfc3339)
    }

    pub fn mark(&self, role: &str, message_id: &str, kind: &str) -> Result<()> {
        ensure_message_id(message_id)?;
        if !self.message_path(role, "new", message_id).exists() {
            bail!("message not in role mailbox: {message_id}");
        }
        let dir = self.state_dir(message_id);
        fs::create_dir_all(&dir)?;
        let mut ts = Utc::now().timestamp_millis();
        loop {
            let path = dir.join(format!("{kind}.{role}.{ts}"));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(_) => return Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => ts += 1,
                Err(err) => return Err(err).context("write marker"),
            }
        }
    }

    pub fn ack(&self, role: &str, message_id: &str) -> Result<()> {
        self.mark(role, message_id, "seen")?;
        self.mark(role, message_id, "ack")
    }

    pub fn claim(&self, role: &str, message_id: &str) -> Result<bool> {
        ensure_message_id(message_id)?;
        if !self.message_path(role, "new", message_id).exists() {
            bail!("message not in role mailbox: {message_id}");
        }
        let dir = self.paths.claims_dir().join(message_id).join(role);
        match fs::create_dir_all(dir.parent().unwrap()) {
            Ok(()) => {}
            Err(err) => return Err(err).context("create claims dir"),
        }
        match fs::create_dir(&dir) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(err) => Err(err).context("claim message"),
        }
    }

    pub fn release(&self, role: &str, message_id: &str) -> Result<bool> {
        ensure_message_id(message_id)?;
        let dir = self.paths.claims_dir().join(message_id).join(role);
        match fs::remove_dir(&dir) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err).context("release claim"),
        }
    }

    pub fn clear_claims_for(&self, role: &str) -> Result<()> {
        let claims = self.paths.claims_dir();
        if !claims.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(claims)? {
            let entry = entry?;
            if !entry.path().is_dir() {
                continue;
            }
            let _ = fs::remove_dir(entry.path().join(role));
        }
        Ok(())
    }

    pub fn unread_count(&self, role: &str) -> Result<usize> {
        let entries = self.list(role, "new")?;
        Ok(entries.iter().filter(|entry| entry.unread).count())
    }

    pub fn thread(&self, role: &str, thread_or_message: &str) -> Result<Vec<MailEntry>> {
        if thread_or_message.len() != 26
            || !thread_or_message.chars().all(|c| c.is_ascii_alphanumeric())
        {
            bail!("invalid thread or message id");
        }
        if let Some(index) = self.open_index() {
            let thread_id = match index.thread_id_for(thread_or_message) {
                Ok(Some(thread_id)) => thread_id,
                Ok(None) => thread_or_message.to_string(),
                Err(_) => {
                    fs::remove_file(self.index_path()).ok();
                    return self.scan_thread(role, thread_or_message);
                }
            };
            match index.thread(role, &thread_id) {
                Ok(rows) => {
                    let mut seen = std::collections::HashSet::new();
                    let mut out = Vec::new();
                    for row in rows {
                        if !seen.insert(row.message_id.clone()) {
                            continue;
                        }
                        out.push(self.entry_from_index(&index, role, row));
                    }
                    return Ok(out);
                }
                Err(_) => {
                    fs::remove_file(self.index_path()).ok();
                }
            }
        }
        self.scan_thread(role, thread_or_message)
    }

    fn scan_thread(&self, role: &str, thread_or_message: &str) -> Result<Vec<MailEntry>> {
        let mut messages: std::collections::BTreeMap<String, (PathBuf, Envelope)> =
            std::collections::BTreeMap::new();
        let mail_root = self.paths.mail_dir();
        if let Ok(roles) = fs::read_dir(&mail_root) {
            for role_entry in roles.flatten() {
                for folder in ["new", "sent"] {
                    let dir = role_entry.path().join(folder);
                    let Ok(entries) = fs::read_dir(&dir) else {
                        continue;
                    };
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|e| e.to_str()) != Some("json") {
                            continue;
                        }
                        let Ok(envelope) = read_json::<Envelope>(&path) else {
                            continue;
                        };
                        messages
                            .entry(envelope.message_id.to_string())
                            .or_insert((path, envelope));
                    }
                }
            }
        }
        let thread_id = messages
            .values()
            .find(|(_, envelope)| envelope.message_id.to_string() == thread_or_message)
            .map(|(_, envelope)| envelope.thread_id.to_string())
            .unwrap_or_else(|| thread_or_message.to_string());

        let mut out = Vec::new();
        for (path, envelope) in messages.values() {
            if envelope.thread_id.to_string() != thread_id {
                continue;
            }
            let message_id = envelope.message_id.to_string();
            let has_copy = self.message_path(role, "new", &message_id).exists()
                || self.message_path(role, "sent", &message_id).exists();
            if !has_copy {
                continue;
            }
            out.push(self.entry_from(role, path, envelope, "thread"));
        }
        out.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.message_id.cmp(&b.message_id))
        });
        Ok(out)
    }

    pub fn clear_role_runtime(&self, role: &str) {
        fs::remove_file(
            self.paths
                .hook_dir()
                .join(format!("{}.json", role.replace('@', "_at_"))),
        )
        .ok();
    }

    pub fn remove_pending_for_role(&self, role: &str) -> Result<()> {
        let dir = self.pending_dir();
        let Ok(entries) = fs::read_dir(&dir) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            if let Ok(pending) = read_json::<Pending>(&entry.path()) {
                if pending.role == role {
                    fs::remove_file(entry.path()).ok();
                }
            }
        }
        Ok(())
    }

    // ---------------------------------------------------------------- diagnostics

    pub fn doctor(&self) -> Result<Vec<DoctorIssue>> {
        let mut issues = Vec::new();
        let mut known: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut message_count = 0usize;

        if let Ok(role_dirs) = fs::read_dir(self.paths.mail_dir()) {
            for role_entry in role_dirs.flatten() {
                let role_path = role_entry.path();
                if !role_path.is_dir() {
                    continue;
                }
                let tmp = role_path.join("tmp");
                if let Ok(entries) = fs::read_dir(&tmp) {
                    for entry in entries.flatten() {
                        let Ok(meta) = entry.metadata() else {
                            continue;
                        };
                        if let Ok(modified) = meta.modified() {
                            if let Ok(age) = modified.elapsed() {
                                if age.as_secs() > (TMP_MAX_AGE_HOURS as u64) * 3600 {
                                    issues.push(DoctorIssue {
                                        code: "stale_tmp".into(),
                                        message: "temporary file older than 36h".into(),
                                        path: entry.path().display().to_string(),
                                    });
                                }
                            }
                        }
                    }
                }
                for folder in ["new", "sent"] {
                    let dir = role_path.join(folder);
                    let Ok(entries) = fs::read_dir(&dir) else {
                        continue;
                    };
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|e| e.to_str()) != Some("json") {
                            continue;
                        }
                        let Ok(meta) = entry.metadata() else {
                            continue;
                        };
                        if meta.len() == 0 {
                            issues.push(DoctorIssue {
                                code: "empty_message".into(),
                                message: "zero-byte message".into(),
                                path: path.display().to_string(),
                            });
                            continue;
                        }
                        match read_json::<Envelope>(&path) {
                            Ok(envelope) => {
                                known.insert(envelope.message_id.to_string());
                                message_count += 1;
                            }
                            Err(_) => issues.push(DoctorIssue {
                                code: "bad_json".into(),
                                message: "message is not valid JSON".into(),
                                path: path.display().to_string(),
                            }),
                        }
                    }
                }
            }
        }

        if read_json::<Config>(&self.config_path()).is_err() {
            issues.push(DoctorIssue {
                code: "bad_config".into(),
                message: "config.json is missing or unreadable".into(),
                path: self.config_path().display().to_string(),
            });
        }

        let index_path = self.index_path();
        if !index_path.exists() {
            if message_count > 0 {
                issues.push(DoctorIssue {
                    code: "index_missing".into(),
                    message: "messages exist but the SQLite index is missing".into(),
                    path: index_path.display().to_string(),
                });
            }
        } else {
            match MailIndex::open(&index_path) {
                Ok(index) => match index.counts() {
                    Ok((indexed, _)) if indexed != message_count => {
                        issues.push(DoctorIssue {
                            code: "index_stale".into(),
                            message: format!(
                                "index has {indexed} message copies, spool has {message_count}"
                            ),
                            path: index_path.display().to_string(),
                        });
                    }
                    Ok(_) => {}
                    Err(_) => issues.push(DoctorIssue {
                        code: "bad_index".into(),
                        message: "cannot read the SQLite index".into(),
                        path: index_path.display().to_string(),
                    }),
                },
                Err(_) => issues.push(DoctorIssue {
                    code: "bad_index".into(),
                    message: "cannot open the SQLite index".into(),
                    path: index_path.display().to_string(),
                }),
            }
        }

        for (dir, code, label) in [
            (
                self.paths.markers_dir(),
                "orphan_state",
                "markers for a missing message",
            ),
            (
                self.paths.claims_dir(),
                "orphan_claim",
                "claim for a missing message",
            ),
        ] {
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if !entry.path().is_dir() {
                    continue;
                }
                let id = entry.file_name().to_string_lossy().to_string();
                if !known.contains(&id) {
                    issues.push(DoctorIssue {
                        code: code.into(),
                        message: label.into(),
                        path: entry.path().display().to_string(),
                    });
                }
            }
        }
        Ok(issues)
    }

    pub fn index_rebuild(&self) -> Result<serde_json::Value> {
        let result = self.rebuild_index();
        if result.is_err() {
            fs::remove_file(self.index_path()).ok();
        }
        result
    }

    fn rebuild_index(&self) -> Result<serde_json::Value> {
        let index = MailIndex::reset(&self.index_path())?;
        let mut mailboxes = 0usize;
        let mut messages = 0usize;
        let mut indexed = 0usize;
        if let Ok(roles) = fs::read_dir(self.paths.mail_dir()) {
            for role_entry in roles.flatten() {
                if !role_entry.path().is_dir() {
                    continue;
                }
                mailboxes += 1;
                let mailbox = role_entry.file_name().to_string_lossy().to_string();
                for folder in ["new", "sent"] {
                    let Ok(entries) = fs::read_dir(role_entry.path().join(folder)) else {
                        continue;
                    };
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|e| e.to_str()) != Some("json") {
                            continue;
                        }
                        messages += 1;
                        let Ok(envelope) = read_json::<Envelope>(&path) else {
                            continue;
                        };
                        index
                            .upsert(&IndexedMessage::from_envelope(&envelope, &mailbox, folder))?;
                        indexed += 1;
                    }
                }
            }
        }
        Ok(json!({"mailboxes": mailboxes, "messages": messages, "indexed": indexed}))
    }

    // ---------------------------------------------------------------- session env

    pub fn session_id_from_env() -> Option<String> {
        for key in [
            "TELEX_SESSION_ID",
            "CLAUDE_CODE_SESSION_ID",
            "CODEX_THREAD_ID",
            "DSH_SESSION_ID",
        ] {
            if let Ok(value) = std::env::var(key) {
                if !value.trim().is_empty() {
                    return Some(value);
                }
            }
        }
        None
    }
}

fn ensure_message_id(value: &str) -> Result<()> {
    if value.len() != 26 || !value.chars().all(|c| c.is_ascii_alphanumeric()) {
        bail!("invalid message id");
    }
    Ok(())
}

fn safe_name(value: &str) -> String {
    value
        .chars()
        .take(128)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn session_key(value: &str) -> String {
    let filtered = safe_name(value);
    if filtered == value {
        filtered
    } else {
        format!("{}-{}", filtered, &sha256_hex(value.as_bytes())[..12])
    }
}

fn now_rfc3339() -> String {
    rfc3339(Utc::now())
}

fn rfc3339(value: DateTime<Utc>) -> String {
    value.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

fn parse_rfc3339(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(value)?;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&tmp, bytes)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600)).ok();
    fs::rename(&tmp, path)?;
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&tmp, bytes)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600)).ok();
    fs::rename(&tmp, path)?;
    Ok(())
}

fn sync_dir(dir: &Path) {
    if let Ok(file) = fs::File::open(dir) {
        let _ = file.sync_all();
    }
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn hash_password(password: &str) -> Result<String> {
    let mut salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);
    let params = scrypt::Params::new(14, 8, 1, 32)?;
    let mut key = [0u8; 32];
    scrypt::scrypt(password.as_bytes(), &salt, &params, &mut key)?;
    Ok(format!(
        "scrypt$16384$8$1${}${}",
        hex::encode(salt),
        hex::encode(key)
    ))
}

fn verify_password(password: &str, stored: &str) -> bool {
    let parts: Vec<&str> = stored.split('$').collect();
    if parts.len() != 6 || parts[0] != "scrypt" {
        return false;
    }
    let n: u64 = parts[1].parse().unwrap_or(0);
    let r: u32 = parts[2].parse().unwrap_or(0);
    let p: u32 = parts[3].parse().unwrap_or(0);
    if n == 0 || !n.is_power_of_two() {
        return false;
    }
    let log_n = n.trailing_zeros() as u8;
    let Ok(salt) = hex::decode(parts[4]) else {
        return false;
    };
    let Ok(expected) = hex::decode(parts[5]) else {
        return false;
    };
    let Ok(params) = scrypt::Params::new(log_n, r, p, expected.len()) else {
        return false;
    };
    let mut key = vec![0u8; expected.len()];
    if scrypt::scrypt(password.as_bytes(), &salt, &params, &mut key).is_err() {
        return false;
    }
    key == expected
}
