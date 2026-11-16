use chrono::{Duration, Utc};
use serde_json::json;
use telex::domain::{Address, Envelope, Intent};

fn addr(input: &str) -> Address {
    Address::parse(input, "local").expect("valid address")
}

fn sample() -> Envelope {
    Envelope::new(
        addr("lead@local"),
        vec![addr("worker@local")],
        "flaky test in ci/payments",
        "It times out about 1 in 20 runs. Can you take a look?",
        Intent::Question,
    )
}

fn fields(envelope: &Envelope) -> Vec<String> {
    envelope
        .validate(None)
        .into_iter()
        .map(|e| e.field)
        .collect()
}

#[test]
fn new_message_has_thread_and_dedupe() {
    let envelope = sample();
    assert_eq!(envelope.thread_id, envelope.message_id);
    assert!(envelope.in_reply_to.is_none());
    assert!(envelope.references.is_empty());
    assert!(envelope.dedupe_key.starts_with("sha256:"));
    assert_eq!(envelope.dedupe_key.len(), "sha256:".len() + 64);
    assert!(envelope.validate(None).is_empty());
}

#[test]
fn reply_inherits_thread_and_subject_prefix() {
    let parent = sample();
    let reply = Envelope::reply_to(
        &parent,
        addr("worker@local"),
        vec![addr("lead@local")],
        "Got it.",
    );
    assert_eq!(reply.thread_id, parent.thread_id);
    assert_eq!(reply.in_reply_to, Some(parent.message_id));
    assert_eq!(reply.references, vec![parent.message_id]);
    assert_eq!(reply.subject, "Re: flaky test in ci/payments");
    assert!(!reply.reply_expected);
    assert!(reply.validate(Some(&parent)).is_empty());
}

#[test]
fn repeated_replies_cap_references_at_twenty() {
    let mut parent = sample();
    for i in 0..25 {
        let from = if i % 2 == 0 {
            "worker@local"
        } else {
            "lead@local"
        };
        let to = if i % 2 == 0 {
            "lead@local"
        } else {
            "worker@local"
        };
        let reply = Envelope::reply_to(&parent, addr(from), vec![addr(to)], "ok");
        assert!(reply.validate(Some(&parent)).is_empty());
        parent = reply;
    }
    assert_eq!(parent.references.len(), 20);
}

#[test]
fn dedupe_tracks_content() {
    let mut envelope = sample();
    let before = envelope.dedupe_key.clone();
    envelope.subject = "a different subject".to_string();
    let after = envelope.compute_dedupe_key();
    assert_ne!(before, after);
    envelope.dedupe_key = after;
    assert!(envelope.validate(None).is_empty());
}

#[test]
fn validation_rejects_bad_fields() {
    let mut envelope = sample();
    envelope.subject = "line\nbreak".to_string();
    assert!(fields(&envelope).contains(&"subject".to_string()));

    let mut envelope = sample();
    envelope.priority = 9;
    assert!(fields(&envelope).contains(&"priority".to_string()));

    let mut envelope = sample();
    envelope.dedupe_key = format!("sha256:{}", "0".repeat(64));
    assert!(fields(&envelope).contains(&"dedupe_key".to_string()));

    let mut envelope = sample();
    envelope.body.bytes += 1;
    assert!(fields(&envelope).contains(&"body.bytes".to_string()));

    let mut envelope = sample();
    envelope.expires_at = Some(envelope.created_at - Duration::seconds(1));
    assert!(fields(&envelope).contains(&"expires_at".to_string()));

    let mut envelope = sample();
    envelope.sig = Some(json!({"x": 1}));
    assert!(fields(&envelope).contains(&"sig".to_string()));
}

#[test]
fn validation_rejects_duplicate_recipients() {
    let mut envelope = sample();
    envelope.cc.push(addr("worker@local"));
    assert!(fields(&envelope).contains(&"to".to_string()));
}

#[test]
fn reply_thread_mismatch_is_rejected() {
    let parent = sample();
    let mut reply = Envelope::reply_to(
        &parent,
        addr("worker@local"),
        vec![addr("lead@local")],
        "Got it.",
    );
    reply.thread_id = ulid::Ulid::new();
    reply.dedupe_key = reply.compute_dedupe_key();
    let errors = reply.validate(Some(&parent));
    assert!(errors.iter().any(|e| e.field == "thread_id"));
}

#[test]
fn serde_roundtrip_and_unknown_field_rejection() {
    let envelope = sample();
    let text = String::from_utf8(envelope.canonical_bytes()).expect("utf8");
    let parsed: Envelope = serde_json::from_str(&text).expect("parse");
    assert_eq!(parsed, envelope);
    assert!(parsed.validate(None).is_empty());

    let mut value: serde_json::Value = serde_json::from_str(&text).expect("json");
    value
        .as_object_mut()
        .expect("object")
        .insert("extra".into(), json!(1));
    assert!(serde_json::from_value::<Envelope>(value).is_err());
}

#[test]
fn payload_depth_is_limited() {
    let mut envelope = sample();
    envelope.payload = Some(
        json!({"a": {"b": {"c": {"d": 1}}}})
            .as_object()
            .expect("object")
            .clone(),
    );
    let errors = envelope.validate(None);
    assert!(errors.iter().any(|e| e.field.starts_with("payload")));
}

#[test]
fn intent_is_closed_enum_string() {
    let text = serde_json::to_string(&Intent::ReviewRequest).expect("serialize");
    assert_eq!(text, "\"review-request\"");
    assert!(serde_json::from_str::<Intent>("\"delete-everything\"").is_err());
}

#[test]
fn expires_must_be_after_created() {
    let mut envelope = sample();
    envelope.expires_at = Some(envelope.created_at + Duration::hours(1));
    envelope.dedupe_key = envelope.compute_dedupe_key();
    assert!(envelope.validate(None).is_empty());

    envelope.expires_at = Some(envelope.created_at);
    assert!(fields(&envelope).contains(&"expires_at".to_string()));

    let future = Utc::now() + Duration::days(7);
    assert!(future > envelope.created_at);
}
