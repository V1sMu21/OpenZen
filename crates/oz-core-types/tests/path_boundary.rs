use serde_json::json;

use oz_core_types::path_trust::outside_workspace_target;

#[test]
fn an_absolute_path_outside_the_workspace_needs_approval() {
    let wd = "/private/tmp/ozecf-e2e-5010";
    let args = json!({"content": "no", "file_path": "/private/tmp/ozecf-outside-5010.txt"});
    let got = outside_workspace_target("write", &args, wd);
    assert!(got.is_some(), "outside target not detected: {got:?}");
}

#[test]
fn a_relative_path_inside_needs_no_approval() {
    let args = json!({"content": "x", "file_path": "notes.txt"});
    assert!(outside_workspace_target("write", &args, "/tmp/ws").is_none());
}

#[test]
fn a_dangling_symlink_is_treated_as_its_target() {
    let ws = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let link = ws.path().join("link");
    let target = outside.path().join("not-yet-created.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let args = serde_json::json!({
        "file_path": link.to_string_lossy(),
        "content": "x",
    });
    let got = outside_workspace_target("write", &args, ws.path().to_str().unwrap());
    assert!(
        got.is_some(),
        "a dangling symlink must resolve to its outside target, got {got:?}"
    );
}
