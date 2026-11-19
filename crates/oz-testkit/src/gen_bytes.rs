//! G2 — wire-byte mutation generator.
//!
//! Seeds are *recipes*, not blobs: a fixture name plus a deterministic list of
//! mutations. The runner re-applies them, so the checked-in corpus stays small
//! and every seed is readable in a diff. Fixtures remain the source of truth.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    Truncate { at: usize },
    DeleteCloser,
    Closer { to: String },
    ReplaceBar,
    PrefixJunk { text: String },
    SuffixJunk { text: String },
    Nul { at: usize },
    Crlf,
    WrapJson,
    HtmlEscape,
    Repeat { times: usize },
    DropFirst,
    DropLast,
    DupField { key: String },
    JsonEscape,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireSeed {
    pub id: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    pub ops: Vec<Op>,
    /// An unmutated fixture must be recoverable; a refusal here means the
    /// repair layer silently stopped working (the mutation corpus alone
    /// cannot see that, because refusing arbitrary garbage is legal).
    #[serde(default)]
    pub must_recover: bool,
}

pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/oz-llm/tests/fixtures")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("crates/oz-llm/tests/fixtures"))
}

pub fn load_fixtures() -> Result<BTreeMap<String, String>, String> {
    let dir = fixtures_dir();
    let mut out = BTreeMap::new();
    let rd = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for e in rd {
        let e = e.map_err(|e| e.to_string())?;
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("txt") {
            continue;
        }
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("non-utf8 fixture name")?
            .to_string();
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        out.insert(name, text);
    }
    if out.is_empty() {
        return Err(format!("no fixtures under {}", dir.display()));
    }
    Ok(out)
}

fn tool_for(fixture: &str) -> Option<&'static str> {
    if fixture.starts_with("ask_user") {
        Some("ask_user")
    } else if fixture.starts_with("write") {
        Some("write")
    } else if fixture.starts_with("code_run") {
        Some("code_run")
    } else if fixture.starts_with("panel") {
        Some("open_side_panel")
    } else {
        None
    }
}

const CLOSERS: &[&str] = &["</", "</parameter>", "</｜DSML", "</個parameter>", ""];
const BARS: &[(&str, &str)] = &[("｜", "|"), ("|", "｜"), ("個", "｜"), ("：", ":")];

/// Deterministic LCG — no `rand`, no `thread_rng`: the corpus must regenerate
/// byte-identically on every machine.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 11
    }
    fn pick(&mut self, n: usize) -> usize {
        (self.next() as usize) % n.max(1)
    }
}

fn push_seed(out: &mut Vec<WireSeed>, limit: usize, source: &str, ops: Vec<Op>) {
    if out.len() >= limit {
        return;
    }
    let id = format!("WB-{:04}", out.len() + 1);
    let must_recover = ops.is_empty();
    out.push(WireSeed {
        id,
        source: source.to_string(),
        tool: tool_for(source).map(str::to_string),
        ops,
        must_recover,
    });
}

pub fn generate(limit: usize) -> Result<Vec<WireSeed>, String> {
    let fixtures = load_fixtures()?;
    let mut out = Vec::new();

    // 1) identity + systematic truncation (every ~1/48 of the file).
    for (name, text) in &fixtures {
        push_seed(&mut out, limit, name, vec![]);
        let step = (text.len() / 48).max(1);
        for at in (1..text.len()).step_by(step) {
            push_seed(&mut out, limit, name, vec![Op::Truncate { at }]);
        }
    }

    // 2) every single operator on every fixture.
    for name in fixtures.keys() {
        push_seed(&mut out, limit, name, vec![Op::DeleteCloser]);
        for to in CLOSERS {
            push_seed(
                &mut out,
                limit,
                name,
                vec![Op::Closer {
                    to: (*to).to_string(),
                }],
            );
        }
        for (from, to) in BARS {
            push_seed(
                &mut out,
                limit,
                name,
                vec![Op::SuffixJunk {
                    text: format!("{from}{to}"),
                }],
            );
        }
        push_seed(&mut out, limit, name, vec![Op::ReplaceBar]);
        push_seed(
            &mut out,
            limit,
            name,
            vec![Op::PrefixJunk {
                text: "{\"response\">".into(),
            }],
        );
        push_seed(
            &mut out,
            limit,
            name,
            vec![Op::SuffixJunk {
                text: "\n</invoke>\n</function_calls>".into(),
            }],
        );
        push_seed(&mut out, limit, name, vec![Op::Nul { at: 1 }]);
        push_seed(&mut out, limit, name, vec![Op::Crlf]);
        push_seed(&mut out, limit, name, vec![Op::WrapJson]);
        push_seed(&mut out, limit, name, vec![Op::HtmlEscape]);
        push_seed(&mut out, limit, name, vec![Op::Repeat { times: 4 }]);
        push_seed(&mut out, limit, name, vec![Op::DropFirst]);
        push_seed(&mut out, limit, name, vec![Op::DropLast]);
        push_seed(&mut out, limit, name, vec![Op::JsonEscape]);
        for key in ["content", "file_path", "code", "question"] {
            push_seed(
                &mut out,
                limit,
                name,
                vec![Op::DupField { key: key.into() }],
            );
        }
    }

    // 3) deterministic multi-op combinations (2–3 ops) until the limit.
    let names: Vec<&String> = fixtures.keys().collect();
    let ops = [
        "truncate",
        "closer",
        "bar",
        "prefix",
        "suffix",
        "nul",
        "crlf",
        "wrap",
        "html",
        "repeat",
        "dropfirst",
        "droplast",
        "dupfield",
        "escape",
    ];
    let mut rng = Lcg(0x0ECF_2026_1117);
    while out.len() < limit {
        let src = names[rng.pick(names.len())].clone();
        let n = 2 + rng.pick(2); // 2 or 3
        let mut chosen = Vec::with_capacity(n);
        for _ in 0..n {
            chosen.push(ops[rng.pick(ops.len())]);
        }
        let text = &fixtures[&src];
        let mut recipe = Vec::new();
        for name in chosen {
            match name {
                "truncate" => recipe.push(Op::Truncate {
                    at: 1 + rng.pick(text.len().max(2)),
                }),
                "closer" => recipe.push(Op::Closer {
                    to: CLOSERS[rng.pick(CLOSERS.len())].to_string(),
                }),
                "bar" => recipe.push(Op::ReplaceBar),
                "prefix" => recipe.push(Op::PrefixJunk {
                    text: "{\"response\">".into(),
                }),
                "suffix" => recipe.push(Op::SuffixJunk {
                    text: "\n</invoke>".into(),
                }),
                "nul" => recipe.push(Op::Nul {
                    at: 1 + rng.pick(text.len().max(2)),
                }),
                "crlf" => recipe.push(Op::Crlf),
                "wrap" => recipe.push(Op::WrapJson),
                "html" => recipe.push(Op::HtmlEscape),
                "repeat" => recipe.push(Op::Repeat {
                    times: 2 + rng.pick(3),
                }),
                "dropfirst" => recipe.push(Op::DropFirst),
                "droplast" => recipe.push(Op::DropLast),
                "dupfield" => recipe.push(Op::DupField {
                    key: "content".into(),
                }),
                _ => recipe.push(Op::JsonEscape),
            }
        }
        push_seed(&mut out, limit, &src, recipe);
    }
    Ok(out)
}

/// Apply a recipe. Every operator is total: no indexing without a bound, no
/// assumption about UTF-8 boundaries (bytes are rebuilt as chars where needed).
pub fn apply(text: &str, ops: &[Op]) -> String {
    let mut s = text.to_string();
    for op in ops {
        match op {
            Op::Truncate { at } => {
                let mut cut = (*at).min(s.len());
                while cut > 0 && !s.is_char_boundary(cut) {
                    cut -= 1;
                }
                s.truncate(cut);
            }
            Op::DeleteCloser => {
                for c in ["</parameter>", "</｜DSML", "</個parameter>"] {
                    if let Some(i) = s.find(c) {
                        s.replace_range(i..i + c.len(), "");
                        break;
                    }
                }
            }
            Op::Closer { to } => {
                for c in ["</parameter>", "</｜DSML", "</個parameter>"] {
                    if let Some(i) = s.find(c) {
                        s.replace_range(i..i + c.len(), to);
                        break;
                    }
                }
            }
            Op::ReplaceBar => {
                let mut out = String::with_capacity(s.len());
                for ch in s.chars() {
                    match ch {
                        '｜' => out.push('|'),
                        '|' => out.push('｜'),
                        '個' => out.push('｜'),
                        _ => out.push(ch),
                    }
                }
                s = out;
            }
            Op::PrefixJunk { text } => s = format!("{text}{s}"),
            Op::SuffixJunk { text } => s.push_str(text),
            Op::Nul { at } => {
                let mut cut = (*at).min(s.len());
                while cut > 0 && !s.is_char_boundary(cut) {
                    cut -= 1;
                }
                s.insert(cut, '\u{0}');
            }
            Op::Crlf => s = s.replace('\n', "\r\n"),
            Op::WrapJson => {
                s = serde_json::json!({ "content": s }).to_string();
            }
            Op::HtmlEscape => s = s.replace('"', "&quot;"),
            Op::Repeat { times } => {
                let base: String = s.chars().take(1024).collect();
                s = base.repeat((*times).clamp(1, 8));
                if s.len() > 8192 {
                    s.truncate(8192);
                }
            }
            Op::DropFirst => {
                if let Some(c) = s.chars().next() {
                    s.drain(..c.len_utf8());
                }
            }
            Op::DropLast => {
                if let Some(c) = s.chars().last() {
                    let n = s.len() - c.len_utf8();
                    s.truncate(n);
                }
            }
            Op::DupField { key } => {
                let needle = format!("\"{key}\"");
                if let Some(i) = s.find(&needle) {
                    let frag: String = s[i..].chars().take(80).collect();
                    s.push(',');
                    s.push_str(&frag);
                }
            }
            Op::JsonEscape => {
                s = s.replace('"', "\\\"").replace('\n', "\\n");
            }
        }
    }
    s
}
