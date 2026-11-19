//! G1 — parameter-space generator.
//!
//! Generates cases whose expectation is derived from the alias catalogue plus
//! the documented move-never-invent contract. Deliberately excludes:
//! - aliases that are also canonical keys of the same tool (`mode` for
//!   `code_run`): their value only moves when it names a language, which is a
//!   separate case with its own rule;
//! - `ls`, whose path-family cross-fill is an implementation quirk.
//!
//! Every case asserts the full output object, so a new key appearing in the
//! output is a failure too (no silent extra fields).

use serde_json::{json, Map, Value};

use crate::aliases::{self, Group};
use crate::case::{Case, CaseStatus, Expect, Input, Layer, Severity, Source, Target, TargetKind};

pub fn generate() -> Vec<Case> {
    let mut out = Vec::new();
    let mut n = 0u32;
    let mut push = |tool: &str,
                    canonical: &str,
                    input: Value,
                    expected: Value,
                    forbid: Vec<String>,
                    note: &str,
                    sev: Severity| {
        n += 1;
        out.push(Case {
            id: format!("TA-{n:03}"),
            rev: 1,
            fc: vec!["FC-B2".into()],
            layer: Layer::Unit,
            target: Target {
                kind: TargetKind::Canonicalize,
                tool: Some(tool.into()),
                working_dir: None,
            },
            input: Input::Json { value: input },
            expect: Expect::Exact {
                value: expected,
                forbid_keys: forbid,
            },
            invariants: vec!["idempotent".into(), "no_invention".into()],
            severity: sev,
            source: Some(Source {
                kind: "contract".into(),
                r#ref: format!("{tool}.{canonical} {note}"),
            }),
            budget_ms: Some(50),
            repro: String::new(), // filled below, once the id exists
            status: CaseStatus::Calibrated,
        });
    };

    for entry in aliases::TOOL_ALIASES {
        let canonicals = aliases::canonical_names(entry.tool);
        for g in entry.groups {
            let usable: Vec<&str> = g
                .aliases
                .iter()
                .copied()
                .filter(|a| !canonicals.iter().any(|c| c == a))
                .take(8)
                .collect();

            for tag in ["str", "num"] {
                for alias in &usable {
                    // `str` → a probe string, `num` → type confusion (number).
                    let value = if tag == "str" {
                        json!(format!("probe-{alias}"))
                    } else {
                        json!(42)
                    };
                    let mut input = Map::new();
                    input.insert((*alias).to_string(), value.clone());
                    let mut expected = Map::new();
                    expected.insert(g.canonical.to_string(), value);
                    push(
                        entry.tool,
                        g.canonical,
                        Value::Object(input),
                        Value::Object(expected),
                        vec![(*alias).to_string()],
                        &format!("alias={alias} value={tag}"),
                        Severity::P1,
                    );
                }
            }

            // Canonical key wins over an alias (never overwrite the payload).
            if let Some(alias) = usable.first() {
                let mut input = Map::new();
                input.insert(g.canonical.to_string(), json!("real"));
                input.insert((*alias).to_string(), json!("alias"));
                let mut expected = Map::new();
                expected.insert(g.canonical.to_string(), json!("real"));
                push(
                    entry.tool,
                    g.canonical,
                    Value::Object(input),
                    Value::Object(expected),
                    vec![(*alias).to_string()],
                    "canonical-wins",
                    Severity::P0,
                );
            }

            // Normalized key variants only where the alias table actually
            // accepts them (`filepath` yes, `oldstring` no).
            if only_variant_supported(g) {
                for variant in variants(g.canonical) {
                    let mut input = Map::new();
                    input.insert(variant.to_string(), json!("x"));
                    let mut expected = Map::new();
                    expected.insert(g.canonical.to_string(), json!("x"));
                    push(
                        entry.tool,
                        g.canonical,
                        Value::Object(input),
                        Value::Object(expected),
                        vec![variant.to_string()],
                        "key-variant",
                        Severity::P2,
                    );
                }
            }
        }

        // Tool-name-as-key → primary parameter.
        let primary = entry.groups[0].canonical;
        let mut input = Map::new();
        input.insert(entry.tool.to_string(), json!("payload"));
        let mut expected = Map::new();
        expected.insert(primary.to_string(), json!("payload"));
        push(
            entry.tool,
            primary,
            Value::Object(input),
            Value::Object(expected),
            vec![entry.tool.to_string()],
            "tool-name-key",
            Severity::P2,
        );
    }

    // Non-object arguments pass through untouched.
    for (label, value) in [
        ("string", json!("ls")),
        ("array", json!([1, 2])),
        ("null", Value::Null),
        ("number", json!(42)),
    ] {
        n += 1;
        out.push(Case {
            id: format!("TA-{n:03}"),
            rev: 1,
            fc: vec!["FC-B2".into()],
            layer: Layer::Unit,
            target: Target {
                kind: TargetKind::Canonicalize,
                tool: Some("write".into()),
                working_dir: None,
            },
            input: Input::Json {
                value: value.clone(),
            },
            expect: Expect::Exact {
                value,
                forbid_keys: vec![],
            },
            invariants: vec!["no_panic".into()],
            severity: Severity::P1,
            source: Some(Source {
                kind: "contract".into(),
                r#ref: format!("non-object {label}"),
            }),
            budget_ms: Some(50),
            repro: String::new(),
            status: CaseStatus::Calibrated,
        });
    }

    // The corrupted-call sentinel must reach the dispatcher untouched.
    n += 1;
    let sentinel = json!({"__oz_corrupted_call": "three calls merged"});
    out.push(Case {
        id: format!("TA-{n:03}"),
        rev: 1,
        fc: vec!["FC-B2".into(), "FC-F1".into()],
        layer: Layer::Unit,
        target: Target {
            kind: TargetKind::Canonicalize,
            tool: Some("write".into()),
            working_dir: None,
        },
        input: Input::Json {
            value: sentinel.clone(),
        },
        expect: Expect::Exact {
            value: sentinel,
            forbid_keys: vec![],
        },
        invariants: vec!["no_panic".into()],
        severity: Severity::P0,
        source: Some(Source {
            kind: "incident".into(),
            r#ref: "2026-10-23 merged calls".into(),
        }),
        budget_ms: Some(50),
        repro: String::new(),
        status: CaseStatus::Calibrated,
    });

    for c in &mut out {
        c.repro = format!(
            "cargo run -p oz-testkit --bin oz-caselab -- run --case {}",
            c.id
        );
    }
    out
}

/// The normalized canonical key must be present in the alias list for a
/// camel/kebab/space/upper variant to be accepted by the implementation.
fn only_variant_supported(g: &Group) -> bool {
    let norm = norm_key(g.canonical);
    g.aliases.iter().any(|a| norm_key(a) == norm)
}

fn variants(key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let camel = key
        .split('_')
        .enumerate()
        .map(|(i, p)| {
            if i == 0 {
                p.to_string()
            } else {
                let mut c = p.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect::<String>();
    out.push(camel);
    out.push(key.replace('_', "-"));
    out.push(key.replace('_', " "));
    out.push(key.to_uppercase());
    out
}

fn norm_key(k: &str) -> String {
    k.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}
