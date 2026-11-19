//! Reference-model test for `SessionStore` persistence.
//!
//! The model is deliberately dumb (a Vec of JSON values). Every save/reload
//! cycle must reproduce it exactly — including a message edited *in place*
//! without changing the message count (FC-A1).

use serde_json::json;

use oz_server::webui::SessionStore;

fn wait(store: &SessionStore) {
    store.wait_persisted(std::time::Duration::from_secs(5));
}

#[test]
fn save_reload_reproduces_messages_and_in_place_edits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.json");

    let mut store = SessionStore::persisted(path.clone());
    store.create_with_id("s1", "model test");
    {
        let e = store.get_mut("s1").expect("session exists");
        e.messages.push(json!({"role": "user", "content": "aaaa"}));
        e.messages
            .push(json!({"role": "assistant", "content": "bbbb"}));
    }
    store.save();
    wait(&store);

    let reloaded = SessionStore::persisted(path.clone());
    let e = reloaded.get("s1").expect("session survived reload");
    assert_eq!(e.messages.len(), 2);
    assert_eq!(e.messages[0]["content"], "aaaa");

    // Same length, different content: a count-only fingerprint would miss it
    // and the old text would come back after a restart.
    {
        let mut store = SessionStore::persisted(path.clone());
        let e = store.get_mut("s1").unwrap();
        e.messages[0]["content"] = json!("cccc");
        store.save();
        wait(&store);
    }
    let reloaded = SessionStore::persisted(path.clone());
    assert_eq!(
        reloaded.get("s1").unwrap().messages[0]["content"],
        "cccc",
        "in-place edit was not persisted"
    );
    drop(reloaded);
    drop(store);
}

#[test]
fn corrupt_file_is_preserved_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.json");
    std::fs::write(&path, b"{ this is not json").unwrap();

    let store = SessionStore::persisted(path.clone());
    assert!(
        store.get("whatever").is_none(),
        "a corrupt file must not resurrect sessions"
    );

    let backup = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().contains("corrupt-"))
        .expect("corrupt file must be renamed to a backup");
    let saved = std::fs::read_to_string(backup.path()).unwrap();
    assert!(saved.contains("this is not json"), "backup content lost");
}

#[test]
fn two_stores_share_one_file_without_losing_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.json");
    let mut a = SessionStore::persisted(path.clone());
    a.create_with_id("s1", "a");
    a.save();
    wait(&a);

    let mut b = SessionStore::persisted(path.clone());
    b.create_with_id("s2", "b");
    b.save();
    wait(&b);

    let reloaded = SessionStore::persisted(path.clone());
    assert!(reloaded.get("s1").is_some(), "writer B erased session s1");
    assert!(reloaded.get("s2").is_some());
}
