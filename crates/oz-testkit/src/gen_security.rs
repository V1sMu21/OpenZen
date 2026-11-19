//! Approval-granularity and write-boundary cases (FC-F2, FC-F3).
//!
//! These are the two places where "the code looks reasonable" is not enough:
//! a pattern that is too coarse grants a whole family of commands, and a
//! boundary check that resolves a different path than the write is a consent
//! mismatch. Both are pure functions, so they belong in the unit corpus.

use serde_json::json;

use crate::case::{Case, CaseStatus, Expect, Input, Layer, Severity, Source, Target, TargetKind};

pub fn generate() -> Vec<Case> {
    let mut out = Vec::new();
    let mut n = 0u32;

    let mut pattern = |tool: &str, code: &str, expect: Expect, ref_: &str, sev: Severity| {
        n += 1;
        out.push(Case {
            id: format!("SC-{n:03}"),
            rev: 1,
            fc: vec!["FC-F2".into()],
            layer: Layer::Unit,
            target: Target {
                kind: TargetKind::Pattern,
                tool: Some(tool.into()),
                working_dir: None,
            },
            input: Input::Json {
                value: json!({ "code": code, "url": code, "file_path": code }),
            },
            expect,
            invariants: vec!["no_panic".into()],
            severity: sev,
            source: Some(Source {
                kind: "contract".into(),
                r#ref: ref_.into(),
            }),
            budget_ms: Some(50),
            repro: String::new(),
            status: CaseStatus::Calibrated,
        });
    };

    // A grant for `git status` must not cover `git clean -fdx`; the whole
    // command is the key, not the first word.
    pattern(
        "code_run",
        "git status",
        Expect::Exact {
            value: json!(["code_run", "git status"]),
            forbid_keys: vec![],
        },
        "whole-command key",
        Severity::P1,
    );
    pattern(
        "code_run",
        "npm install --save-dev typescript",
        Expect::Exact {
            value: json!(["code_run", "npm install --save-dev typescript"]),
            forbid_keys: vec![],
        },
        "whitespace normalized",
        Severity::P2,
    );
    // Newline-joined commands are never auto-trustable: the pattern is a
    // content hash, so two different scripts cannot share a grant.
    pattern(
        "code_run",
        "git status\ngit clean -fdx",
        Expect::Contains {
            text: vec!["!unsafe:".into()],
        },
        "newline forces content-addressed key",
        Severity::P0,
    );
    pattern(
        "code_run",
        &"x".repeat(5_000),
        Expect::Contains {
            text: vec!["!long:".into()],
        },
        "oversized command is content-addressed",
        Severity::P1,
    );
    pattern(
        "read",
        "/Users/x/report.md",
        Expect::Exact {
            value: json!(["read", "/Users/x/report.md"]),
            forbid_keys: vec![],
        },
        "full path, not the first two segments",
        Severity::P1,
    );
    pattern(
        "write",
        "work/a.txt",
        Expect::Exact {
            value: json!(["write", "work/a.txt"]),
            forbid_keys: vec![],
        },
        "relative path key",
        Severity::P2,
    );
    pattern(
        "web_scan",
        "https://Example.COM/path?q=1",
        Expect::Exact {
            value: json!(["web_scan", "example.com"]),
            forbid_keys: vec![],
        },
        "host is lowercased",
        Severity::P2,
    );
    pattern(
        "web_js",
        "location.href",
        Expect::Exact {
            value: json!(["web_js", "*"]),
            forbid_keys: vec![],
        },
        "all JS shares one wildcard (and is not auto-trusted)",
        Severity::P1,
    );
    pattern(
        "mcp__foo__bar",
        "",
        Expect::Exact {
            value: json!(["mcp__foo__bar", "mcp__foo__bar"]),
            forbid_keys: vec![],
        },
        "MCP tools key on their full name",
        Severity::P2,
    );

    // Write boundary (FC-F3).
    let mut boundary = |tool: &str, path: &str, expect: Expect, ref_: &str, sev: Severity| {
        n += 1;
        out.push(Case {
            id: format!("SC-{n:03}"),
            rev: 1,
            fc: vec!["FC-F3".into()],
            layer: Layer::Unit,
            target: Target {
                kind: TargetKind::Outside,
                tool: Some(tool.into()),
                working_dir: Some("/tmp/ws".into()),
            },
            input: Input::Json {
                value: json!({ "file_path": path, "content": "x" }),
            },
            expect,
            invariants: vec!["no_panic".into()],
            severity: sev,
            source: Some(Source {
                kind: "contract".into(),
                r#ref: ref_.into(),
            }),
            budget_ms: Some(50),
            repro: String::new(),
            status: CaseStatus::Calibrated,
        });
    };
    boundary(
        "write",
        "/private/tmp/ecf-outside.txt",
        Expect::Some,
        "absolute outside",
        Severity::P0,
    );
    boundary(
        "write",
        "notes.txt",
        Expect::None,
        "relative inside",
        Severity::P1,
    );
    boundary(
        "write",
        "../escape.txt",
        Expect::Some,
        "traversal",
        Severity::P0,
    );
    boundary(
        "write",
        "/tmp/ws/inside.txt",
        Expect::None,
        "absolute inside",
        Severity::P1,
    );

    for c in &mut out {
        c.repro = format!(
            "cargo run -p oz-testkit --bin oz-caselab -- run --case {}",
            c.id
        );
    }
    out
}
