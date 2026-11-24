use std::collections::BTreeMap;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use ulid::Ulid;

use crate::domain::address::Address;
use crate::domain::body::Body;
use crate::domain::canonical::{canonical_json, sha256_hex};
use crate::domain::intent::{Disposition, Intent};

pub const VERSION: &str = "0.1";
const MAX_SUBJECT: usize = 998;
const MAX_BODY: usize = 1024 * 1024;
const MAX_PAYLOAD: usize = 4096;
const MAX_REFERENCES: usize = 20;
const MAX_RECIPIENTS: usize = 16;
const MAX_HEADERS: usize = 16;
const MAX_HEADER_VALUE: usize = 512;
const MAX_DISPLAY: usize = 64;

/// Sender.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sender {
    pub address: Address,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hop {
    pub address: Address,
    #[serde(with = "rfc3339_ms")]
    pub at: DateTime<Utc>,
}

/// One message. Field order does not matter; serialization sorts keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub telex_version: String,
    pub message_id: Ulid,
    pub thread_id: Ulid,
    pub in_reply_to: Option<Ulid>,
    pub references: Vec<Ulid>,
    pub from: Sender,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub subject: String,
    #[serde(with = "rfc3339_ms")]
    pub created_at: DateTime<Utc>,
    pub disposition: Disposition,
    pub intent: Intent,
    pub reply_expected: bool,
    pub priority: u8,
    #[serde(with = "rfc3339_ms_opt")]
    pub expires_at: Option<DateTime<Utc>>,
    pub payload: Option<Map<String, Value>>,
    pub body: Body,
    pub dedupe_key: String,
    pub headers: BTreeMap<String, String>,
    pub hops: Vec<Hop>,
    pub sig: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

impl ValidationError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for ValidationError {}

impl Envelope {
    /// Create a new message with defaults and a computed dedupe key.
    pub fn new(
        from: Address,
        to: Vec<Address>,
        subject: impl Into<String>,
        body_text: &str,
        intent: Intent,
    ) -> Self {
        let id = Ulid::new();
        let now = now_ms();
        let mut envelope = Self {
            telex_version: VERSION.to_string(),
            message_id: id,
            thread_id: id,
            in_reply_to: None,
            references: Vec::new(),
            from: Sender {
                address: from,
                display: None,
            },
            to,
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.into(),
            created_at: now,
            disposition: intent.default_disposition(),
            intent,
            reply_expected: intent.expects_reply(),
            priority: 1,
            expires_at: None,
            payload: None,
            body: Body::from_text(body_text),
            dedupe_key: String::new(),
            headers: BTreeMap::new(),
            hops: Vec::new(),
            sig: None,
        };
        envelope.dedupe_key = envelope.compute_dedupe_key();
        envelope
    }

    /// Reply to a parent: inherit the thread, extend references, prefix Re:.
    pub fn reply_to(parent: &Envelope, from: Address, to: Vec<Address>, body_text: &str) -> Self {
        let mut references = parent.references.clone();
        references.push(parent.message_id);
        if references.len() > MAX_REFERENCES {
            let drop = references.len() - MAX_REFERENCES;
            references.drain(0..drop);
        }
        let subject = if parent.subject.to_ascii_lowercase().starts_with("re:") {
            parent.subject.clone()
        } else {
            format!("Re: {}", parent.subject)
        };
        let mut envelope = Self::new(from, to, subject, body_text, Intent::Answer);
        envelope.thread_id = parent.thread_id;
        envelope.in_reply_to = Some(parent.message_id);
        envelope.references = references;
        envelope.disposition = Disposition::Reply;
        envelope.reply_expected = false;
        envelope.priority = parent.priority;
        envelope.dedupe_key = envelope.compute_dedupe_key();
        envelope
    }

    /// Content hash used for idempotent delivery. Identity and clock fields
    /// are excluded so resending the same logical message gets the same key.
    pub fn compute_dedupe_key(&self) -> String {
        let mut value = serde_json::to_value(self).expect("envelope serializes");
        if let Some(object) = value.as_object_mut() {
            for key in [
                "dedupe_key",
                "sig",
                "message_id",
                "thread_id",
                "created_at",
                "hops",
            ] {
                object.remove(key);
            }
        }
        format!("sha256:{}", sha256_hex(canonical_json(&value).as_bytes()))
    }

    /// Canonical bytes with a trailing newline, as stored on disk.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let value = serde_json::to_value(self).expect("envelope serializes");
        let mut bytes = canonical_json(&value).into_bytes();
        bytes.push(b'\n');
        bytes
    }

    pub fn validate(&self, parent: Option<&Envelope>) -> Vec<ValidationError> {
        let mut errors = Vec::new();

        if self.telex_version != VERSION {
            errors.push(ValidationError::new(
                "telex_version",
                format!("version must be {VERSION}"),
            ));
        }

        if self.subject.is_empty() || self.subject.chars().count() > MAX_SUBJECT {
            errors.push(ValidationError::new(
                "subject",
                "subject must be 1..=998 characters",
            ));
        }
        if self
            .subject
            .chars()
            .any(|c| c == '\r' || c == '\n' || (c.is_control() && c != '\t'))
        {
            errors.push(ValidationError::new(
                "subject",
                "subject must not contain newlines or control characters",
            ));
        }

        if self.to.is_empty() {
            errors.push(ValidationError::new(
                "to",
                "at least one direct recipient is required",
            ));
        }
        let recipients = self.to.len() + self.cc.len() + self.bcc.len();
        if recipients > MAX_RECIPIENTS {
            errors.push(ValidationError::new(
                "to",
                format!("recipients must not exceed {MAX_RECIPIENTS}"),
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for address in self.to.iter().chain(self.cc.iter()).chain(self.bcc.iter()) {
            if !seen.insert(address.as_str()) {
                errors.push(ValidationError::new(
                    "to",
                    format!("duplicate recipient: {address}"),
                ));
            }
        }

        if let Some(display) = &self.from.display {
            if display.chars().count() > MAX_DISPLAY {
                errors.push(ValidationError::new(
                    "from.display",
                    format!("display name must not exceed {MAX_DISPLAY} characters"),
                ));
            }
            if display
                .chars()
                .any(|c| c == '\r' || c == '\n' || c.is_control())
            {
                errors.push(ValidationError::new(
                    "from.display",
                    "display name must not contain newlines or control characters",
                ));
            }
        }

        if self.priority > 3 {
            errors.push(ValidationError::new(
                "priority",
                "priority must be between 0 and 3",
            ));
        }

        if self.disposition != self.intent.default_disposition() {
            errors.push(ValidationError::new(
                "disposition",
                "disposition does not match intent",
            ));
        }
        if self.reply_expected != self.intent.expects_reply() {
            errors.push(ValidationError::new(
                "reply_expected",
                "reply_expected does not match intent",
            ));
        }

        if self.references.len() > MAX_REFERENCES {
            errors.push(ValidationError::new(
                "references",
                format!("references must not exceed {MAX_REFERENCES}"),
            ));
        }
        let mut refs = std::collections::HashSet::new();
        for reference in &self.references {
            if !refs.insert(*reference) {
                errors.push(ValidationError::new(
                    "references",
                    "references contain duplicates",
                ));
            }
        }

        if let Some(payload) = &self.payload {
            validate_payload(payload, 0, "payload", &mut errors);
            let size = serde_json::to_string(payload)
                .map(|s| s.len())
                .unwrap_or(usize::MAX);
            if size > MAX_PAYLOAD {
                errors.push(ValidationError::new(
                    "payload",
                    format!("payload must not exceed {MAX_PAYLOAD} bytes"),
                ));
            }
        }

        match self.body.decode() {
            Ok(raw) => {
                if raw.len() != self.body.bytes {
                    errors.push(ValidationError::new(
                        "body.bytes",
                        "body.bytes does not match the decoded length",
                    ));
                }
                if raw.len() > MAX_BODY {
                    errors.push(ValidationError::new(
                        "body",
                        format!("body must not exceed {MAX_BODY} bytes"),
                    ));
                }
                if std::str::from_utf8(&raw).is_err() {
                    errors.push(ValidationError::new("body.data", "body is not valid UTF-8"));
                }
            }
            Err(_) => errors.push(ValidationError::new(
                "body.data",
                "body is not valid base64",
            )),
        }

        if let Some(expires) = self.expires_at {
            if expires <= self.created_at {
                errors.push(ValidationError::new(
                    "expires_at",
                    "expires_at must be after created_at",
                ));
            }
        }

        let expected = self.compute_dedupe_key();
        if self.dedupe_key != expected {
            errors.push(ValidationError::new(
                "dedupe_key",
                "dedupe_key does not match the content",
            ));
        }

        if self.headers.len() > MAX_HEADERS {
            errors.push(ValidationError::new(
                "headers",
                format!("headers must not exceed {MAX_HEADERS}"),
            ));
        }
        for (key, value) in &self.headers {
            if key.is_empty()
                || key.len() > 64
                || !key
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                errors.push(ValidationError::new(
                    "headers",
                    format!("invalid header key: {key}"),
                ));
            }
            if value.len() > MAX_HEADER_VALUE {
                errors.push(ValidationError::new(
                    "headers",
                    format!("header value too long: {key}"),
                ));
            }
            if value.contains('\r') || value.contains('\n') {
                errors.push(ValidationError::new(
                    "headers",
                    format!("header value must not contain newlines: {key}"),
                ));
            }
        }

        if !self.hops.is_empty() {
            errors.push(ValidationError::new(
                "hops",
                "v0.1 does not support forwarding; hops must be empty",
            ));
        }
        if self.sig.is_some() {
            errors.push(ValidationError::new(
                "sig",
                "v0.1 does not support signing; sig must be null",
            ));
        }

        if self.in_reply_to.is_none() {
            if self.thread_id != self.message_id {
                errors.push(ValidationError::new(
                    "thread_id",
                    "new message thread_id must equal message_id",
                ));
            }
            if !self.references.is_empty() {
                errors.push(ValidationError::new(
                    "references",
                    "new message references must be empty",
                ));
            }
        } else if let Some(parent) = parent {
            if self.message_id == parent.message_id {
                errors.push(ValidationError::new("message_id", "cannot reply to self"));
            }
            if self.in_reply_to != Some(parent.message_id) {
                errors.push(ValidationError::new(
                    "in_reply_to",
                    "in_reply_to does not match the parent",
                ));
            }
            if self.thread_id != parent.thread_id {
                errors.push(ValidationError::new(
                    "thread_id",
                    "thread_id does not match the parent thread",
                ));
            }
            let mut expected_refs = parent.references.clone();
            expected_refs.push(parent.message_id);
            if expected_refs.len() > MAX_REFERENCES {
                let drop = expected_refs.len() - MAX_REFERENCES;
                expected_refs.drain(0..drop);
            }
            if self.references != expected_refs {
                errors.push(ValidationError::new(
                    "references",
                    "references do not match the derived parent chain",
                ));
            }
        }

        errors
    }
}

fn validate_payload(
    payload: &Map<String, Value>,
    depth: usize,
    field: &str,
    errors: &mut Vec<ValidationError>,
) {
    if depth >= 3 {
        errors.push(ValidationError::new(
            field,
            "payload nesting depth must not exceed 3",
        ));
        return;
    }
    for (key, value) in payload {
        let valid_key = key.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && key
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !valid_key || key.len() > 32 {
            errors.push(ValidationError::new(
                format!("{field}.{key}"),
                "invalid payload key",
            ));
        }
        match value {
            Value::Object(nested) => {
                validate_payload(nested, depth + 1, &format!("{field}.{key}"), errors)
            }
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    if let Value::Object(nested) = item {
                        validate_payload(
                            nested,
                            depth + 1,
                            &format!("{field}.{key}[{index}]"),
                            errors,
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

fn now_ms() -> DateTime<Utc> {
    let now = Utc::now();
    DateTime::from_timestamp_millis(now.timestamp_millis()).expect("valid timestamp")
}

mod rfc3339_ms {
    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &DateTime<Utc>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<DateTime<Utc>, D::Error> {
        let raw = String::deserialize(deserializer)?;
        DateTime::parse_from_rfc3339(&raw)
            .map(|value| value.with_timezone(&Utc))
            .map_err(serde::de::Error::custom)
    }
}

mod rfc3339_ms_opt {
    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<DateTime<Utc>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(value) => {
                serializer.serialize_some(&value.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
            }
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<DateTime<Utc>>, D::Error> {
        let raw = Option::<String>::deserialize(deserializer)?;
        match raw {
            None => Ok(None),
            Some(raw) => DateTime::parse_from_rfc3339(&raw)
                .map(|value| Some(value.with_timezone(&Utc)))
                .map_err(serde::de::Error::custom),
        }
    }
}
