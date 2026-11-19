//! G5 — spec → scenario drafts.
//!
//! This is the *deterministic* half of the generator: it mines the acceptance
//! criteria tables into draft cases that a human (or an explicitly enabled LLM
//! pass) must turn into a machine-checkable oracle. Drafts are never executed
//! and never counted as passing — they exist so spec coverage is visible.

use std::collections::BTreeSet;
use std::path::Path;

use crate::case::{Case, CaseStatus, Expect, Input, Layer, Severity, Source, Target, TargetKind};

pub fn mine(md: &Path) -> Result<Vec<Case>, String> {
    let text = std::fs::read_to_string(md).map_err(|e| format!("{}: {e}", md.display()))?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if !line.starts_with('|') || line.contains("---") {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() < 3 {
            continue;
        }
        let status = cells
            .iter()
            .find(|c| c.contains('✅') || c.contains('⚠') || c.contains('❌'));
        let Some(status) = status else { continue };
        let title = cells
            .iter()
            .take(3)
            .filter(|c| !c.is_empty() && !c.contains('✅') && !c.contains('⚠') && !c.contains('❌'))
            .cloned()
            .collect::<Vec<_>>()
            .join(" · ");
        if title.len() < 8 || title.contains("Status") || title.contains("Feature") {
            continue;
        }
        let key = title.to_lowercase();
        if !seen.insert(key) {
            continue;
        }
        let n = out.len() + 1;
        out.push(Case {
            id: format!("SP-{n:03}"),
            rev: 1,
            fc: vec!["FC-I1".into()],
            layer: Layer::Spec,
            target: Target {
                kind: TargetKind::E2e,
                tool: None,
                working_dir: None,
            },
            input: Input::None,
            expect: Expect::Some,
            invariants: vec!["needs_calibration".into()],
            severity: Severity::P2,
            source: Some(Source {
                kind: "spec".into(),
                r#ref: format!("{}:{} {}", md.display(), lineno + 1, status),
            }),
            budget_ms: Some(60_000),
            repro: String::new(),
            status: CaseStatus::Draft,
        });
    }
    if out.is_empty() {
        return Err(format!("no scorable rows found in {}", md.display()));
    }
    Ok(out)
}

pub fn write_drafts(cases: &[Case], dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    // Remove stale drafts so a shrinking spec does not leave zombies behind.
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.path().extension().and_then(|x| x.to_str()) == Some("json") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    for c in cases {
        let path = dir.join(format!("{}.json", c.id));
        let mut s = serde_json::to_string_pretty(c).map_err(|e| e.to_string())?;
        s.push('\n');
        std::fs::write(&path, s).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}
