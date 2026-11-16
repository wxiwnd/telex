use chrono::{Duration, Utc};
use telex::domain::{Address, Envelope, Intent};
use telex::store::Store;
use tempfile::TempDir;

fn store() -> (TempDir, Store) {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::new(dir.path());
    store.init("admin-password-123", "local").expect("init");
    (dir, store)
}

fn addr(value: &str) -> Address {
    Address::parse(value, "local").expect("address")
}

fn envelope(to: &str) -> Envelope {
    Envelope::new(
        addr("lead@local"),
        vec![addr(to)],
        "hello",
        "body",
        Intent::Note,
    )
}

#[test]
fn delivers_bcc_cleared_and_keeps_sent_copy() {
    let (_dir, store) = store();
    let mut envelope = envelope("worker@local");
    envelope.bcc = vec![addr("secret@local")];
    envelope.dedupe_key = envelope.compute_dedupe_key();

    let report = store
        .deliver(&envelope, &["worker@local".into(), "secret@local".into()])
        .expect("deliver");
    assert_eq!(report.delivered.len(), 2);

    let worker = store
        .read_message("worker@local", &envelope.message_id.to_string(), false)
        .expect("read");
    assert!(worker.envelope.bcc.is_empty());

    let sent = store.list("lead@local", "sent").expect("sent");
    assert_eq!(sent.len(), 1);
    let stored = store
        .read_message("lead@local", &envelope.message_id.to_string(), false)
        .expect("read sent");
    assert_eq!(stored.envelope.bcc.len(), 1);
}

#[test]
fn duplicate_delivery_is_idempotent() {
    let (_dir, store) = store();
    let envelope = envelope("worker@local");
    let first = store
        .deliver(&envelope, &["worker@local".into()])
        .expect("first");
    assert_eq!(first.delivered.len(), 1);
    let second = store
        .deliver(&envelope, &["worker@local".into()])
        .expect("second");
    assert!(second.delivered.is_empty());
    assert_eq!(second.duplicates, vec!["worker@local".to_string()]);
}

#[test]
fn same_message_id_with_different_content_conflicts() {
    let (_dir, store) = store();
    let envelope = envelope("worker@local");
    store
        .deliver(&envelope, &["worker@local".into()])
        .expect("first");

    let mut changed = envelope.clone();
    changed.subject = "different".into();
    changed.dedupe_key = changed.compute_dedupe_key();
    let error = store
        .deliver(&changed, &["worker@local".into()])
        .expect_err("conflict");
    assert!(error.to_string().contains("id-conflict"));
}

#[test]
fn markers_and_ack_are_per_recipient() {
    let (_dir, store) = store();
    let envelope = envelope("worker@local");
    store
        .deliver(&envelope, &["worker@local".into()])
        .expect("deliver");
    let id = envelope.message_id.to_string();

    assert_eq!(store.unread_count("worker@local").expect("unread"), 1);
    store.mark("worker@local", &id, "seen").expect("seen");
    assert_eq!(store.unread_count("worker@local").expect("unread"), 0);
    assert!(store.has_marker(&id, "seen", "worker@local"));
    assert!(!store.has_marker(&id, "seen", "lead@local"));

    store.ack("worker@local", &id).expect("ack");
    assert!(store.has_marker(&id, "ack", "worker@local"));

    let sent = store.list("lead@local", "sent").expect("sent");
    let worker = &sent[0].recipients[0];
    assert_eq!(worker.address, "worker@local");
    assert!(worker.delivered);
    assert!(worker.seen_at.is_some());
    assert!(worker.acked_at.is_some());
}

#[test]
fn claim_is_exclusive_until_released() {
    let (_dir, store) = store();
    let envelope = envelope("worker@local");
    store
        .deliver(&envelope, &["worker@local".into()])
        .expect("deliver");
    let id = envelope.message_id.to_string();

    assert!(store.claim("worker@local", &id).expect("claim"));
    assert!(!store.claim("worker@local", &id).expect("claim again"));
    assert!(store.release("worker@local", &id).expect("release"));
    assert!(store.claim("worker@local", &id).expect("claim again"));
}

#[test]
fn binding_is_one_role_one_session_and_reassignment_revokes_old_token() {
    let (_dir, store) = store();
    store
        .add_role(&addr("worker@local"), None, vec![])
        .expect("role");

    let first = store
        .bind_session("worker@local", "session-a", "claude")
        .expect("bind a");
    let token = store.read_token_file("session-a").expect("token file");
    assert!(
        store
            .verify_token("session-a", &token)
            .expect("verify")
            .is_some()
    );
    assert_eq!(
        store
            .role_by_session("session-a")
            .expect("role")
            .unwrap()
            .address,
        "worker@local"
    );
    assert_eq!(first.role, "worker@local");

    store
        .bind_session("worker@local", "session-b", "codex")
        .expect("bind b");
    assert!(store.read_token_file("session-a").is_none());
    assert!(
        store
            .verify_token("session-a", &token)
            .expect("verify")
            .is_none()
    );
    assert!(store.role_by_session("session-b").expect("role").is_some());
}

#[test]
fn thread_includes_participants_only() {
    let (_dir, store) = store();
    let parent = envelope("worker@local");
    store
        .deliver(&parent, &["worker@local".into()])
        .expect("deliver parent");
    let received = store
        .read_message("worker@local", &parent.message_id.to_string(), false)
        .expect("read")
        .envelope;
    let reply = Envelope::reply_to(
        &received,
        addr("worker@local"),
        vec![addr("lead@local")],
        "done",
    );
    store
        .deliver(&reply, &["lead@local".into()])
        .expect("deliver reply");

    let thread = store
        .thread("lead@local", &parent.message_id.to_string())
        .expect("thread");
    assert_eq!(thread.len(), 2);
    assert!(
        store
            .thread("outsider@local", &parent.message_id.to_string())
            .expect("thread")
            .is_empty()
    );
}

#[test]
fn doctor_reports_corrupt_and_orphans() {
    let (_dir, store) = store();
    let envelope = envelope("worker@local");
    store
        .deliver(&envelope, &["worker@local".into()])
        .expect("deliver");

    std::fs::write(
        store.root().join("mail/worker@local/new/broken.json"),
        "{not json",
    )
    .expect("write");
    std::fs::write(store.root().join("mail/worker@local/new/empty.json"), "").expect("write");
    std::fs::create_dir_all(store.root().join("state/orphan-id")).expect("mkdir");

    let issues = store.doctor().expect("doctor");
    assert!(issues.iter().any(|issue| issue.code == "bad_json"));
    assert!(issues.iter().any(|issue| issue.code == "empty_message"));
    assert!(issues.iter().any(|issue| issue.code == "orphan_state"));
}

#[test]
fn message_ids_and_folders_cannot_escape_the_mailbox() {
    let (_dir, store) = store();
    let envelope = envelope("victim@local");
    store
        .deliver(&envelope, &["victim@local".into()])
        .expect("deliver");
    let id = envelope.message_id.to_string();

    let traversal = format!("../../victim@local/new/{id}");
    assert!(
        store
            .read_message("attacker@local", &traversal, false)
            .is_err()
    );
    assert!(store.list("attacker@local", "../victim@local/new").is_err());
}

#[test]
fn bcc_recipients_see_the_thread_they_received() {
    let (_dir, store) = store();
    let mut envelope = envelope("worker@local");
    envelope.bcc = vec![addr("secret@local")];
    envelope.dedupe_key = envelope.compute_dedupe_key();
    let id = envelope.message_id.to_string();
    store
        .deliver(&envelope, &["worker@local".into(), "secret@local".into()])
        .expect("deliver");

    let thread = store.thread("secret@local", &id).expect("thread");
    assert_eq!(thread.len(), 1);
    assert!(thread[0].bcc.is_empty());
    assert!(
        store
            .thread("outsider@local", &id)
            .expect("thread")
            .is_empty()
    );
}

#[test]
fn one_session_can_only_represent_one_role() {
    let (_dir, store) = store();
    store
        .add_role(&addr("a@local"), None, vec![])
        .expect("role a");
    store
        .add_role(&addr("b@local"), None, vec![])
        .expect("role b");

    store
        .bind_session("a@local", "shared", "claude")
        .expect("bind a");
    store
        .bind_session("b@local", "shared", "claude")
        .expect("bind b");

    assert!(
        store
            .get_role("a@local")
            .expect("role a")
            .expect("exists")
            .session_id
            .is_none()
    );
    assert_eq!(
        store
            .get_role("b@local")
            .expect("role b")
            .expect("exists")
            .session_id,
        Some("shared".to_string())
    );
    assert_eq!(
        store
            .role_by_session("shared")
            .expect("session")
            .expect("bound")
            .address,
        "b@local"
    );
}

#[test]
fn read_receipts_do_not_match_similar_address_prefixes() {
    let (_dir, store) = store();
    let mut envelope = envelope("a@local");
    envelope.cc = vec![addr("a@local.b")];
    envelope.dedupe_key = envelope.compute_dedupe_key();
    let id = envelope.message_id.to_string();
    store
        .deliver(&envelope, &["a@local".into(), "a@local.b".into()])
        .expect("deliver");

    store.mark("a@local.b", &id, "seen").expect("seen");
    assert!(!store.has_marker(&id, "seen", "a@local"));
    assert!(store.has_marker(&id, "seen", "a@local.b"));

    let sent = store.list("lead@local", "sent").expect("sent");
    let plain = sent[0]
        .recipients
        .iter()
        .find(|item| item.address == "a@local")
        .expect("plain");
    let dotted = sent[0]
        .recipients
        .iter()
        .find(|item| item.address == "a@local.b")
        .expect("dotted");
    assert!(plain.seen_at.is_none());
    assert!(dotted.seen_at.is_some());
}

#[test]
fn expired_messages_do_not_count_as_unread() {
    let (_dir, store) = store();
    let mut envelope = envelope("worker@local");
    let now = Utc::now();
    envelope.created_at = now - Duration::hours(2);
    envelope.expires_at = Some(now - Duration::hours(1));
    envelope.dedupe_key = envelope.compute_dedupe_key();
    store
        .deliver(&envelope, &["worker@local".into()])
        .expect("deliver");

    let entries = store.list("worker@local", "new").expect("inbox");
    assert!(entries[0].expired);
    assert!(!entries[0].unread);
    assert_eq!(store.unread_count("worker@local").expect("count"), 0);
}

#[test]
fn wake_failures_are_reported_by_doctor() {
    let (_dir, store) = store();
    store
        .record_wake_failure("worker@local", "01ARZ3NDEKTSV4RRFFQ69G5FAV", 4, "exit 1")
        .expect("record");
    let issues = store.doctor().expect("doctor");
    assert!(issues.iter().any(|issue| issue.code == "wake_failed"));
    store.clear_wake_failure("worker@local");
    assert!(
        !store
            .doctor()
            .expect("doctor")
            .iter()
            .any(|issue| issue.code == "wake_failed")
    );
}

#[test]
fn session_file_names_do_not_collide_after_sanitizing() {
    let (_dir, store) = store();
    assert_ne!(store.token_path("a/b"), store.token_path("a_b"));
    assert_ne!(store.token_path("a/b"), store.token_path("a-b"));
}
