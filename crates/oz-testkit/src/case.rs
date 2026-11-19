//! Case IR — one case is one reproducible experiment.
//!
//! Storage is JSONL (one object per line) for generated bulk cases and JSON
//! for hand-written drafts. The IR is deliberately small: an input, an
//! expectation **kind**, and enough provenance to explain why the case exists.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

/// Known failure classes (single source: `openzen/references/failure-classes.md`).
pub const FC_CLASSES: &[&str] = &[
    "FC-A1", "FC-A2", "FC-A3", "FC-A4", "FC-A5", "FC-B1", "FC-B2", "FC-B3", "FC-B4", "FC-C1",
    "FC-C2", "FC-C3", "FC-D1", "FC-D2", "FC-D3", "FC-D4", "FC-D5", "FC-E1", "FC-E2", "FC-E3",
    "FC-F1", "FC-F2", "FC-F3", "FC-F4", "FC-G1", "FC-G2", "FC-G3", "FC-H1", "FC-H2", "FC-H3",
    "FC-H4", "FC-I1", "FC-I2", "FC-I3",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    Unit,
    Prop,
    Model,
    E2e,
    Ui,
    Spec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Severity {
    P0,
    P1,
    #[default]
    P2,
    P3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CaseStatus {
    #[default]
    Calibrated,
    Draft,
    Retired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    /// `oz_core_types::canonicalize_tool_args`
    Canonicalize,
    /// `oz_llm::stream::repair_parameter_style_args`
    Repair,
    /// `oz_llm::stream::normalize_tool_args_for`
    Normalize,
    /// `oz_core_types::missing_arg_error` (exact error wording)
    MissingArg,
    /// `oz_safety::patterns::build_pattern` — approval granularity.
    Pattern,
    /// `oz_core_types::path_trust::outside_workspace_target` — write boundary.
    Outside,
    /// Executed by the Python E2E matrix, not by this crate.
    E2e,
    /// A frontend sequence, executed by vitest.
    Ui,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Target {
    pub kind: TargetKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Boundary root for `Outside` cases (defaults to `/tmp/ws`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Input {
    Json {
        value: Value,
    },
    Text {
        text: String,
    },
    /// For `missing_arg`: which keys the call did send.
    Args {
        value: Value,
    },
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Expect {
    /// Byte/JSON-identical output (key order ignored, array order kept).
    Exact {
        value: Value,
        #[serde(default)]
        forbid_keys: Vec<String>,
    },
    /// Structured error / text must contain every fragment.
    Contains { text: Vec<String> },
    /// Must recover something (used by the fixture properties).
    Some,
    /// Must refuse to recover.
    None,
    /// The six wire properties from the plan (see `props.rs`).
    Properties,
    /// Tool error must name the given missing keys and the sent key list.
    MissingArg {
        missing: Vec<String>,
        #[serde(default)]
        sent: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Source {
    pub kind: String,
    #[serde(default)]
    pub r#ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Case {
    pub id: String,
    #[serde(default = "one")]
    pub rev: u32,
    #[serde(default)]
    pub fc: Vec<String>,
    pub layer: Layer,
    pub target: Target,
    pub input: Input,
    pub expect: Expect,
    #[serde(default)]
    pub invariants: Vec<String>,
    #[serde(default)]
    pub severity: Severity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_ms: Option<u64>,
    #[serde(default)]
    pub repro: String,
    #[serde(default)]
    pub status: CaseStatus,
}

fn one() -> u32 {
    1
}

impl Case {
    /// Hard validation. Any error makes `oz-casegen --check` fail (exit 4):
    /// a case whose oracle is missing must never enter the corpus.
    pub fn validate(&self) -> Vec<String> {
        let mut errs = Vec::new();
        if !valid_id(&self.id) {
            errs.push(format!("{}: id must look like AREA-001", self.id));
        }
        if self.rev == 0 {
            errs.push(format!("{}: rev must be >= 1", self.id));
        }
        if self.layer != Layer::Spec && self.fc.is_empty() {
            errs.push(format!(
                "{}: fc must not be empty (or use layer=spec)",
                self.id
            ));
        }
        for fc in &self.fc {
            if !FC_CLASSES.contains(&fc.as_str()) {
                errs.push(format!("{}: unknown fc class {fc}", self.id));
            }
        }
        match (&self.target.kind, &self.input) {
            (TargetKind::Canonicalize, Input::Json { .. }) => {}
            (TargetKind::Normalize, Input::Json { .. }) => {}
            (TargetKind::Repair, Input::Text { .. }) => {}
            (TargetKind::MissingArg, Input::Args { .. }) => {}
            (TargetKind::Pattern | TargetKind::Outside, Input::Json { .. }) => {}
            (TargetKind::E2e | TargetKind::Ui, _) => {}
            (kind, input) => errs.push(format!(
                "{}: target {kind:?} cannot consume input {input:?}",
                self.id
            )),
        }
        if matches!(
            self.target.kind,
            TargetKind::Canonicalize | TargetKind::Repair | TargetKind::Normalize
        ) && self.target.tool.is_none()
        {
            errs.push(format!(
                "{}: tool is required for {:?}",
                self.id, self.target.kind
            ));
        }
        match &self.expect {
            Expect::Exact { forbid_keys, .. } if forbid_keys.iter().any(|k| k.is_empty()) => {
                errs.push(format!("{}: empty key in forbid_keys", self.id))
            }
            Expect::Contains { text } if text.is_empty() => {
                errs.push(format!("{}: contains.text must not be empty", self.id))
            }
            Expect::MissingArg { missing, .. } if missing.is_empty() => errs.push(format!(
                "{}: missing_arg.missing must not be empty",
                self.id
            )),
            _ => {}
        }
        // A no-op oracle is worse than no case: it reports a green detector
        // for its FC class while asserting nothing.
        let oracle_ok = match self.target.kind {
            TargetKind::Canonicalize => {
                matches!(&self.expect, Expect::Exact { .. } | Expect::Contains { .. })
            }
            TargetKind::Normalize => matches!(&self.expect, Expect::Exact { .. }),
            TargetKind::Repair => matches!(
                &self.expect,
                Expect::Exact { .. }
                    | Expect::Contains { .. }
                    | Expect::Some
                    | Expect::None
                    | Expect::Properties
            ),
            TargetKind::MissingArg => matches!(&self.expect, Expect::MissingArg { .. }),
            TargetKind::Pattern => {
                matches!(&self.expect, Expect::Exact { .. } | Expect::Contains { .. })
            }
            TargetKind::Outside => {
                matches!(
                    &self.expect,
                    Expect::Some | Expect::None | Expect::Exact { .. }
                )
            }
            TargetKind::E2e | TargetKind::Ui => true,
        };
        if !oracle_ok {
            errs.push(format!(
                "{}: oracle {:?} cannot judge target {:?}",
                self.id, self.expect, self.target.kind
            ));
        }
        if self.layer != Layer::Spec
            && self.status == CaseStatus::Calibrated
            && self.repro.is_empty()
        {
            errs.push(format!(
                "{}: calibrated case needs a repro command",
                self.id
            ));
        }
        if let Some(b) = self.budget_ms {
            if b == 0 {
                errs.push(format!("{}: budget_ms must be > 0", self.id));
            }
        }
        errs
    }

    pub fn fingerprint(&self) -> String {
        let mut h = 0u64;
        for b in serde_json::to_vec(self).unwrap_or_default() {
            h = h.wrapping_mul(1099511628211).wrapping_add(b as u64);
        }
        format!("{h:016x}")
    }
}

fn valid_id(id: &str) -> bool {
    let mut parts = id.split('-');
    let (Some(area), Some(num), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    (2..=6).contains(&area.len())
        && area.chars().all(|c| c.is_ascii_uppercase())
        && num.len() == 3
        && num.chars().all(|c| c.is_ascii_digit())
}

/// Read a JSONL case file, collecting per-line parse errors instead of
/// silently dropping malformed lines (FC-G3).
pub fn read_jsonl(path: &std::path::Path) -> Result<Vec<Case>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let case: Case =
            serde_json::from_str(line).map_err(|e| format!("{}:{}: {e}", path.display(), i + 1))?;
        out.push(case);
    }
    Ok(out)
}

pub fn write_jsonl(path: &std::path::Path, cases: &[Case]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut out = String::new();
    for c in cases {
        out.push_str(&serde_json::to_string(c).map_err(|e| e.to_string())?);
        out.push('\n');
    }
    std::fs::write(path, out).map_err(|e| format!("{}: {e}", path.display()))
}

/// Duplicate ids across all loaded files (a duplicate makes the report lie
/// about per-case history).
pub fn duplicate_ids<'a>(cases: impl IntoIterator<Item = &'a Case>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut dup = BTreeSet::new();
    for c in cases {
        if !seen.insert(c.id.clone()) {
            dup.insert(c.id.clone());
        }
    }
    dup.into_iter().collect()
}
