//! Derived SQLite index for the mail spool.
//!
//! The spool stays authoritative. The index is rebuilt from it and may be
//! deleted at any time; readers fall back to scanning when it is missing or
//! unreadable. `dedupe_key` is a content hash of the envelope so `doctor` can
//! detect index rows that still exist but no longer match the spool.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

use crate::domain::Envelope;

pub const SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS mail (
    message_id   TEXT NOT NULL,
    mailbox      TEXT NOT NULL,
    folder       TEXT NOT NULL,
    thread_id    TEXT NOT NULL,
    from_addr    TEXT NOT NULL,
    to_json      TEXT NOT NULL,
    cc_json      TEXT NOT NULL,
    bcc_json     TEXT NOT NULL,
    subject      TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    intent       TEXT NOT NULL,
    disposition  TEXT NOT NULL,
    priority     INTEGER NOT NULL,
    expires_at   TEXT,
    dedupe_key   TEXT NOT NULL,
    PRIMARY KEY (message_id, mailbox, folder)
);
CREATE INDEX IF NOT EXISTS mail_by_mailbox
    ON mail(mailbox, folder, created_at);
CREATE INDEX IF NOT EXISTS mail_by_thread
    ON mail(mailbox, thread_id, created_at);
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedMessage {
    pub message_id: String,
    pub mailbox: String,
    pub folder: String,
    pub thread_id: String,
    pub from_addr: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub created_at: String,
    pub intent: String,
    pub disposition: String,
    pub priority: u8,
    pub expires_at: Option<String>,
    pub dedupe_key: String,
}

impl IndexedMessage {
    pub fn from_envelope(envelope: &Envelope, mailbox: &str, folder: &str) -> Self {
        Self {
            message_id: envelope.message_id.to_string(),
            mailbox: mailbox.to_string(),
            folder: folder.to_string(),
            thread_id: envelope.thread_id.to_string(),
            from_addr: envelope.from.address.to_string(),
            to: envelope.to.iter().map(|a| a.to_string()).collect(),
            cc: envelope.cc.iter().map(|a| a.to_string()).collect(),
            bcc: envelope.bcc.iter().map(|a| a.to_string()).collect(),
            subject: envelope.subject.clone(),
            created_at: rfc3339(envelope.created_at),
            intent: enum_name(&envelope.intent),
            disposition: enum_name(&envelope.disposition),
            priority: envelope.priority,
            expires_at: envelope.expires_at.map(rfc3339),
            dedupe_key: envelope.compute_dedupe_key(),
        }
    }
}

pub struct MailIndex {
    conn: Connection,
}

impl MailIndex {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(2))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version != SCHEMA_VERSION {
            bail!("unsupported index schema version: {version}");
        }
        Ok(Self { conn })
    }

    pub fn create(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).ok();
        conn.busy_timeout(Duration::from_secs(2))?;
        conn.execute_batch(SCHEMA)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Self { conn })
    }

    pub fn reset(path: &Path) -> Result<Self> {
        std::fs::remove_file(path).ok();
        Self::create(path)
    }

    pub fn upsert(&self, message: &IndexedMessage) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO mail (
                message_id, mailbox, folder, thread_id, from_addr,
                to_json, cc_json, bcc_json, subject, created_at,
                intent, disposition, priority, expires_at, dedupe_key
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                message.message_id,
                message.mailbox,
                message.folder,
                message.thread_id,
                message.from_addr,
                json_strings(&message.to),
                json_strings(&message.cc),
                json_strings(&message.bcc),
                message.subject,
                message.created_at,
                message.intent,
                message.disposition,
                message.priority,
                message.expires_at,
                message.dedupe_key,
            ],
        )?;
        Ok(())
    }

    pub fn list(&self, mailbox: &str, folder: &str) -> Result<Vec<IndexedMessage>> {
        let mut statement = self.conn.prepare(
            "SELECT message_id, mailbox, folder, thread_id, from_addr,
                    to_json, cc_json, bcc_json, subject, created_at,
                    intent, disposition, priority, expires_at, dedupe_key
             FROM mail
             WHERE mailbox = ?1 AND folder = ?2
             ORDER BY created_at DESC, message_id DESC",
        )?;
        let rows = statement.query_map(params![mailbox, folder], row_to_message)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn thread(&self, mailbox: &str, thread_id: &str) -> Result<Vec<IndexedMessage>> {
        let mut statement = self.conn.prepare(
            "SELECT message_id, mailbox, folder, thread_id, from_addr,
                    to_json, cc_json, bcc_json, subject, created_at,
                    intent, disposition, priority, expires_at, dedupe_key
             FROM mail
             WHERE mailbox = ?1 AND thread_id = ?2
             ORDER BY created_at ASC, message_id ASC",
        )?;
        let rows = statement.query_map(params![mailbox, thread_id], row_to_message)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn thread_id_for(&self, message_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT thread_id FROM mail WHERE message_id = ?1 LIMIT 1",
                params![message_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn find_dedupe(
        &self,
        mailbox: &str,
        folder: &str,
        dedupe_key: &str,
    ) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT message_id FROM mail
                 WHERE mailbox = ?1 AND folder = ?2 AND dedupe_key = ?3
                 LIMIT 1",
                params![mailbox, folder, dedupe_key],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn has_copy(&self, mailbox: &str, message_id: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM mail
             WHERE mailbox = ?1 AND message_id = ?2 AND folder = 'new'",
            params![mailbox, message_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn all(&self) -> Result<Vec<IndexedMessage>> {
        let mut statement = self.conn.prepare(
            "SELECT message_id, mailbox, folder, thread_id, from_addr,
                    to_json, cc_json, bcc_json, subject, created_at,
                    intent, disposition, priority, expires_at, dedupe_key
             FROM mail",
        )?;
        let rows = statement.query_map([], row_to_message)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn counts(&self) -> Result<(usize, usize)> {
        let (messages, mailboxes): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COUNT(DISTINCT mailbox) FROM mail",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((messages as usize, mailboxes as usize))
    }
}

fn row_to_message(row: &Row<'_>) -> rusqlite::Result<IndexedMessage> {
    let to_json: String = row.get("to_json")?;
    let cc_json: String = row.get("cc_json")?;
    let bcc_json: String = row.get("bcc_json")?;
    Ok(IndexedMessage {
        message_id: row.get("message_id")?,
        mailbox: row.get("mailbox")?,
        folder: row.get("folder")?,
        thread_id: row.get("thread_id")?,
        from_addr: row.get("from_addr")?,
        to: parse_strings(&to_json),
        cc: parse_strings(&cc_json),
        bcc: parse_strings(&bcc_json),
        subject: row.get("subject")?,
        created_at: row.get("created_at")?,
        intent: row.get("intent")?,
        disposition: row.get("disposition")?,
        priority: row.get("priority")?,
        expires_at: row.get("expires_at")?,
        dedupe_key: row.get("dedupe_key")?,
    })
}

fn parse_strings(text: &str) -> Vec<String> {
    serde_json::from_str(text).unwrap_or_default()
}

fn json_strings(values: &[String]) -> String {
    serde_json::to_string(values).unwrap_or_else(|_| "[]".to_string())
}

fn enum_name<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

fn rfc3339(value: DateTime<Utc>) -> String {
    value.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}
