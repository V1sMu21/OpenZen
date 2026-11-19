//! Acceptance-gate composition.
//!
//! The composer never asks a human and never trusts the model: it derives
//! gates from the workspace (does a build/test exist?) and from the task's own
//! deliverables (was something actually written?). Everything it cannot make
//! executable becomes a Review gate, which fails closed to `unverified`.

use std::path::Path;

use crate::goal::spec::GoalGate;

fn command(cmd: &str) -> GoalGate {
    GoalGate::Command {
        command: cmd.into(),
        timeout_secs: 300,
        retries: 0,
    }
}

/// Lines of the form `[verify] <command>` are the task author's explicit
/// acceptance checks. They are strong evidence by construction (the author
/// chose them), bounded and deduplicated so a prompt-injected goal cannot add
/// unlimited commands.
pub fn explicit_verification_gates(objective: &str) -> Vec<GoalGate> {
    let mut out: Vec<GoalGate> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in objective.lines() {
        let trimmed = line.trim();
        if !trimmed.to_ascii_lowercase().starts_with("[verify]") {
            continue;
        }
        let command = trimmed[8..].trim();
        if command.is_empty() || command.len() > 2_000 || !seen.insert(command.to_string()) {
            continue;
        }
        out.push(GoalGate::StrongCommand {
            command: command.to_string(),
            timeout_secs: 300,
        });
        if out.len() >= 8 {
            break;
        }
    }
    out
}

fn has_test_script(package_json: &str) -> (bool, bool) {
    let parsed: serde_json::Value = serde_json::from_str(package_json).unwrap_or_default();
    let scripts = parsed.get("scripts");
    let test = scripts
        .and_then(|s| s.get("test"))
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.contains("no test specified"));
    let build = scripts
        .and_then(|s| s.get("build"))
        .and_then(|v| v.as_str())
        .is_some();
    (test, build)
}

/// The strongest cheap signal available for this workspace, if any.
pub fn repo_gate(workspace: &Path) -> Option<GoalGate> {
    if workspace.join("Cargo.toml").is_file() {
        return Some(command("cargo check --quiet"));
    }
    if let Ok(raw) = std::fs::read_to_string(workspace.join("package.json")) {
        let (test, build) = has_test_script(&raw);
        if test {
            return Some(command("npm test --silent"));
        }
        if build {
            return Some(command("npm run build --silent"));
        }
    }
    if workspace.join("pyproject.toml").is_file()
        || workspace.join("pytest.ini").is_file()
        || workspace.join("tox.ini").is_file()
    {
        return Some(command("python3 -m pytest -q"));
    }
    if workspace.join("go.mod").is_file() {
        return Some(command("go build ./..."));
    }
    if let Ok(raw) = std::fs::read_to_string(workspace.join("Makefile")) {
        if raw.lines().any(|l| l.trim_start().starts_with("test:")) {
            return Some(command("make test"));
        }
    }
    None
}

/// A tamper check for build configuration captured when the task is created.
/// A worker that edits Cargo.toml/package.json right before verification can
/// otherwise turn a failing build into a passing one. The command is a normal
/// gate, so a mismatch fails verification instead of being ignored.
fn build_config_gate(workspace: &Path) -> Option<GoalGate> {
    use sha2::{Digest, Sha256};
    let files = [
        "Cargo.toml",
        "Cargo.lock",
        "package.json",
        "package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "Makefile",
        "pyproject.toml",
        "go.mod",
        "go.sum",
    ];
    let mut checks = Vec::new();
    for name in files {
        let path = workspace.join(name);
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let mut h = Sha256::new();
        h.update(&bytes);
        let digest = format!("{:x}", h.finalize());
        checks.push(format!(
            "test \"$(shasum -a 256 {name} | awk '{{print $1}}')\" = \"{digest}\""
        ));
    }
    if checks.is_empty() {
        return None;
    }
    Some(command(&checks.join(" && ")))
}

/// Stable literal tokens from the objective that a deliverable must contain.
/// Only explicit, checkable tokens: `P0`/`P1`… plus a few request nouns.
pub fn objective_tokens(objective: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let bytes: Vec<char> = objective.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if (bytes[i] == 'P' || bytes[i] == 'p')
            && i + 1 < bytes.len()
            && bytes[i + 1].is_ascii_digit()
        {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() && j - i <= 3 {
                j += 1;
            }
            let token: String = bytes[i..j].iter().collect();
            let token = token.to_uppercase();
            if !out.contains(&token) {
                out.push(token);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    for word in ["对比", "实验", "基准"] {
        if objective.contains(word) && !out.iter().any(|t| t == word) {
            out.push(word.to_string());
        }
    }
    out.truncate(4);
    out
}

fn is_document(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".md") || lower.ends_with(".html") || lower.ends_with(".txt")
}

pub fn is_research_objective(objective: &str) -> bool {
    [
        "调研", "取证", "审计", "定位", "来源", "证据", "判定", "分析", "根因", "对比",
    ]
    .iter()
    .any(|k| objective.contains(k))
}

/// Gates for what the run claims to have produced.
pub fn document_gates(objective: &str, deliverables: &[String]) -> Vec<GoalGate> {
    let tokens = objective_tokens(objective);
    let mut out = Vec::new();
    for path in deliverables.iter().take(8) {
        if is_document(path) {
            // Reports get a real size floor; a plain .txt only has to be
            // non-empty (an 800-byte floor rejected a legitimate 5-byte file
            // in the live E2E).
            let lower = path.to_ascii_lowercase();
            let min_bytes = if lower.ends_with(".md") || lower.ends_with(".html") {
                800
            } else {
                1
            };
            out.push(GoalGate::FileContent {
                path: path.clone(),
                contains_all: tokens.clone(),
                not_contains: vec!["TODO".into(), "待补".into(), "unimplemented!".into()],
                min_bytes: Some(min_bytes),
            });
            // Research reports need independent source evidence; a text
            // assertion alone can never be verified (0 strong / 1 weak).
            if is_research_objective(objective) {
                out.push(GoalGate::SourceCheck {
                    path: path.clone(),
                    min_sources: 3,
                });
            }
        } else {
            out.push(GoalGate::FileAssertion {
                path: path.clone(),
                must_exist: true,
                contains: None,
            });
        }
    }
    out
}

/// Full composition. Never returns a Human gate: if nothing is executable the
/// task still runs, and its result can only be `unverified`.
pub fn compose(objective: &str, workspace: &Path, deliverables: &[String]) -> Vec<GoalGate> {
    let mut gates = Vec::new();
    if let Some(g) = repo_gate(workspace) {
        gates.push(g);
    }
    if let Some(g) = build_config_gate(workspace) {
        gates.push(g);
    }
    gates.extend(explicit_verification_gates(objective));
    gates.extend(document_gates(objective, deliverables));
    if gates.is_empty() {
        gates.push(GoalGate::Review {
            rubric: format!(
                "Does the delivered work satisfy the task: {}? Name concrete defects only.",
                objective.chars().take(400).collect::<String>()
            ),
            min_confidence: 0.6,
        });
    }
    gates
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn cargo_workspace_gets_a_build_gate() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        let gates = compose("修好构建", dir.path(), &[]);
        assert!(matches!(gates[0], GoalGate::Command { .. }));
        assert!(!gates.iter().any(GoalGate::has_human));
    }

    #[test]
    fn build_config_tamper_check_is_captured() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        let gates = compose("修好构建", dir.path(), &[]);
        assert!(
            gates.iter().any(|g| matches!(g, GoalGate::Command { command, .. } if command.contains("shasum -a 256 Cargo.toml"))),
            "a build-config hash gate must be present: {gates:?}"
        );
    }

    #[test]
    fn research_reports_get_a_source_check_gate() {
        let dir = tempdir().unwrap();
        let gates = compose(
            "调研 zcode 会话并产出取证清单",
            dir.path(),
            &["docs/sources.md".into()],
        );
        assert!(
            gates.iter().any(
                |g| matches!(g, GoalGate::SourceCheck { min_sources, .. } if *min_sources == 3)
            ),
            "research task must get a strong source gate: {gates:?}"
        );
    }

    #[test]
    fn explicit_verify_lines_become_strong_gates() {
        let dir = tempdir().unwrap();
        let gates = compose(
            "写报告\n[verify] cargo test --quiet a_ok\n[verify] cargo check --quiet",
            dir.path(),
            &[],
        );
        let strong = gates
            .iter()
            .filter(|g| matches!(g, GoalGate::StrongCommand { .. }))
            .count();
        assert_eq!(strong, 2, "{gates:?}");
        assert!(gates.iter().all(|g| !matches!(g, GoalGate::Review { .. })));
    }

    #[test]
    fn empty_workspace_falls_back_to_review_not_human() {
        let dir = tempdir().unwrap();
        let gates = compose("写一份说明", dir.path(), &[]);
        assert!(matches!(gates[0], GoalGate::Review { .. }));
        assert!(!gates.iter().any(GoalGate::has_human));
    }

    #[test]
    fn documents_get_content_gates_with_objective_tokens() {
        let dir = tempdir().unwrap();
        let gates = compose(
            "把 P0/P1/P2 做完并给出对比",
            dir.path(),
            &["docs/report.md".into()],
        );
        match &gates[0] {
            GoalGate::FileContent {
                contains_all,
                not_contains,
                min_bytes,
                ..
            } => {
                assert!(contains_all.contains(&"P0".to_string()));
                assert!(contains_all.contains(&"对比".to_string()));
                assert!(not_contains.contains(&"TODO".to_string()));
                assert_eq!(*min_bytes, Some(800));
            }
            other => panic!("unexpected gate: {other:?}"),
        }
    }

    #[test]
    fn plain_text_files_only_need_to_be_non_empty() {
        let dir = tempdir().unwrap();
        let gates = compose("写一个 hello 文件", dir.path(), &["a.txt".into()]);
        match &gates[0] {
            GoalGate::FileContent { min_bytes, .. } => assert_eq!(*min_bytes, Some(1)),
            other => panic!("unexpected gate: {other:?}"),
        }
    }

    #[test]
    fn package_json_uses_test_then_build() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"test":"vitest run"}}"#,
        )
        .unwrap();
        let g = repo_gate(dir.path()).unwrap();
        match g {
            GoalGate::Command { command, .. } => assert!(command.contains("npm test")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn objective_tokens_are_stable_and_bounded() {
        assert_eq!(
            objective_tokens("P0 P1 P2 对比 实验"),
            vec!["P0", "P1", "P2", "对比"]
        );
        assert!(objective_tokens("普通任务").is_empty());
    }
}
