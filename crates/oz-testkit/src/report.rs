//! Result model + JSONL/HTML reporter.
//!
//! The reporter never turns `error` or `skip` into `pass`. Exit codes are part
//! of the contract: 1 = product defect, 3 = infrastructure, 4 = corpus.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pass,
    Fail,
    Error,
    Skip,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub case_id: String,
    pub layer: String,
    pub fc: Vec<String>,
    pub severity: String,
    pub status: Status,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
    #[serde(default)]
    pub signature: String,
    #[serde(default)]
    pub repro: String,
    #[serde(default)]
    pub known: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorInfo {
    pub kind: String,
    pub message: String,
}

impl Outcome {
    pub fn pass(case_id: &str, layer: &str, fc: &[String], severity: &str, ms: u64) -> Self {
        Self {
            case_id: case_id.into(),
            layer: layer.into(),
            fc: fc.to_vec(),
            severity: severity.into(),
            status: Status::Pass,
            duration_ms: ms,
            expected: None,
            actual: None,
            error: None,
            signature: String::new(),
            repro: String::new(),
            known: false,
        }
    }

    pub fn fail(
        mut self,
        expected: serde_json::Value,
        actual: serde_json::Value,
        msg: &str,
    ) -> Self {
        self.status = Status::Fail;
        self.expected = Some(expected);
        self.actual = Some(actual);
        self.error = Some(ErrorInfo {
            kind: "assertion".into(),
            message: msg.into(),
        });
        self.signature = signature(&self.case_id, "assertion", msg);
        self
    }

    pub fn error(mut self, kind: &str, msg: &str) -> Self {
        self.status = Status::Error;
        self.signature = signature(&self.case_id, kind, msg);
        self.error = Some(ErrorInfo {
            kind: kind.into(),
            message: msg.into(),
        });
        self
    }

    pub fn skip(mut self, why: &str) -> Self {
        self.status = Status::Skip;
        self.error = Some(ErrorInfo {
            kind: "skip".into(),
            message: why.into(),
        });
        self
    }

    pub fn with_repro(mut self, repro: &str) -> Self {
        self.repro = repro.into();
        self
    }
}

fn signature(case_id: &str, kind: &str, message: &str) -> String {
    let family = case_id.split('-').next().unwrap_or(case_id);
    let mut h = 1469598103934665603u64;
    for b in format!("{family}|{kind}|{}", normalize(message)).bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    format!("{h:016x}")
}

/// Drop the volatile parts (paths, durations) so the same defect groups
/// across runs and machines.
fn normalize(msg: &str) -> String {
    let mut out = String::new();
    for w in msg.split_whitespace() {
        if w.starts_with('/') || w.starts_with("target/") {
            out.push_str("<path> ");
        } else {
            out.push_str(w);
            out.push(' ');
        }
    }
    out.trim().to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub tier: String,
    pub started_unix: u64,
    pub outcomes: Vec<Outcome>,
}

impl Run {
    pub fn new(tier: &str) -> Self {
        Self {
            tier: tier.into(),
            started_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            outcomes: Vec::new(),
        }
    }

    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let mut p = 0;
        let mut f = 0;
        let mut e = 0;
        let mut s = 0;
        for o in &self.outcomes {
            match o.status {
                Status::Pass => p += 1,
                Status::Fail => f += 1,
                Status::Error => e += 1,
                Status::Skip => s += 1,
            }
        }
        (p, f, e, s)
    }

    pub fn exit_code(&self) -> i32 {
        if self.outcomes.is_empty() {
            return 4; // an empty run is a corpus error, never a pass
        }
        let (_, f, e, _) = self.counts();
        if e > 0 {
            3
        } else if f > 0 {
            1
        } else {
            0
        }
    }

    pub fn apply_baseline(&mut self, baseline: &BTreeSet<String>) {
        for o in &mut self.outcomes {
            if !o.signature.is_empty() {
                o.known = baseline.contains(&o.signature);
            }
        }
    }

    pub fn write(&self, dir: &Path) -> Result<(PathBuf, PathBuf), String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let jsonl = dir.join("results.jsonl");
        let mut body = String::new();
        for o in &self.outcomes {
            body.push_str(&serde_json::to_string(o).map_err(|e| e.to_string())?);
            body.push('\n');
        }
        std::fs::write(&jsonl, body).map_err(|e| format!("{}: {e}", jsonl.display()))?;
        let html = dir.join("report.html");
        std::fs::write(&html, self.to_html()).map_err(|e| format!("{}: {e}", html.display()))?;
        Ok((jsonl, html))
    }

    pub fn to_html(&self) -> String {
        let (p, f, e, s) = self.counts();
        let total = self.outcomes.len();
        let verdict = if total == 0 {
            ("❌ 空运行（0 用例）", "#e0707a")
        } else if e > 0 {
            ("❌ 基础设施错误", "#e0707a")
        } else if f > 0 {
            ("❌ 阻塞", "#e0707a")
        } else if s > 0 {
            ("⚠️ 通过（含跳过）", "#d9a441")
        } else {
            ("✅ 全部通过", "#5fd3a3")
        };
        let mut fc: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for o in &self.outcomes {
            for cls in &o.fc {
                let slot = fc.entry(cls.as_str()).or_default();
                slot.0 += 1;
                if o.status == Status::Pass {
                    slot.1 += 1;
                }
            }
        }
        let mut failures: Vec<&Outcome> = self
            .outcomes
            .iter()
            .filter(|o| o.status != Status::Pass)
            .collect();
        failures.sort_by(|a, b| b.severity.cmp(&a.severity).then(a.case_id.cmp(&b.case_id)));
        let mut slow: Vec<&Outcome> = self.outcomes.iter().collect();
        slow.sort_by_key(|o| std::cmp::Reverse(o.duration_ms));

        let mut h = String::new();
        h.push_str(
            "<!DOCTYPE html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\">\
             <title>ECF 运行报告</title><style>\
             :root{--bg:#0b0d10;--panel:#11161d;--text:#c3cbd8;--gold:#cbb26a;--green:#5fd3a3;\
             --red:#e0707a;--amber:#d9a441;--border:#232a35;--mono:ui-monospace,Menlo,monospace}\
             body{margin:0;background:var(--bg);color:var(--text);font:14px/1.7 -apple-system,\
             \"PingFang SC\",sans-serif}.wrap{max-width:1100px;margin:0 auto;padding:40px 28px}\
             h1{font-size:30px;color:#e9e2cf;margin:0 0 6px}h2{color:var(--gold);font-size:19px;\
             margin:38px 0 10px;border-top:1px solid var(--border);padding-top:22px}\
             code,pre{font-family:var(--mono);font-size:12px}pre{background:#0f141b;\
             border:1px solid var(--border);border-left:3px solid var(--gold);padding:12px;\
             overflow:auto;white-space:pre-wrap}table{width:100%;border-collapse:collapse;\
             font-size:12.6px}th{background:#141a22;color:var(--gold);text-align:left}\
             th,td{border:1px solid var(--border);padding:7px 9px;vertical-align:top}\
             tr:nth-child(even) td{background:#0e1218}.kpi{display:inline-block;margin-right:26px}\
             .kpi b{display:block;font:24px var(--mono);color:var(--gold)}.muted{color:#818d9e}\
             .pill{display:inline-block;padding:0 8px;border-radius:20px;border:1px solid var(--border);\
             font:11px var(--mono)}.fail{color:var(--red)}.pass{color:var(--green)}\
             .warn{color:var(--amber)}</style></head><body><div class=\"wrap\">",
        );
        h.push_str(&format!(
            "<div class=\"muted\">OpenZen · 边界用例工厂 · tier={} · unix={}</div>\
             <h1>运行报告</h1><p style=\"font-size:16px;color:{}\">{}</p>",
            esc(&self.tier),
            self.started_unix,
            verdict.1,
            verdict.0
        ));
        h.push_str(&format!(
            "<p><span class=\"kpi\"><b>{total}</b>用例</span>\
             <span class=\"kpi\"><b class=\"pass\">{p}</b>通过</span>\
             <span class=\"kpi\"><b class=\"fail\">{f}</b>失败</span>\
             <span class=\"kpi\"><b class=\"warn\">{e}</b>基础设施错误</span>\
             <span class=\"kpi\"><b>{s}</b>跳过</span></p>",
        ));

        h.push_str(
            "<h2>FC 覆盖</h2><table><tr><th>类别</th><th>用例</th><th>通过</th><th>状态</th></tr>",
        );
        for (cls, (n, ok)) in &fc {
            let (txt, color) = if ok == n {
                ("有检测器", "pass")
            } else {
                ("有失败", "fail")
            };
            h.push_str(&format!(
                "<tr><td><code>{}</code></td><td>{n}</td><td>{ok}</td>\
                 <td class=\"{color}\">{txt}</td></tr>",
                esc(cls)
            ));
        }
        h.push_str("</table>");

        h.push_str(&format!("<h2>失败与错误（{}）</h2>", failures.len()));
        if failures.is_empty() {
            h.push_str("<p class=\"muted\">无。</p>");
        } else {
            h.push_str("<table><tr><th>用例</th><th>状态</th><th>严重级</th><th>签名</th><th>信息</th><th>复现</th></tr>");
            for o in &failures {
                let msg = o
                    .error
                    .as_ref()
                    .map(|x| x.message.clone())
                    .unwrap_or_default();
                h.push_str(&format!(
                    "<tr><td><code>{}</code></td><td class=\"fail\">{:?}</td><td>{}</td>\
                     <td><code>{}</code>{}</td><td>{}</td><td><code>{}</code></td></tr>",
                    esc(&o.case_id),
                    o.status,
                    esc(&o.severity),
                    esc(&o.signature),
                    if o.known {
                        ""
                    } else {
                        " <span class=\"pill\">NEW</span>"
                    },
                    esc(&msg),
                    esc(&o.repro)
                ));
            }
            h.push_str("</table>");
        }

        h.push_str("<h2>最慢 10 条</h2><table><tr><th>用例</th><th>耗时 ms</th><th>状态</th></tr>");
        for o in slow.iter().take(10) {
            h.push_str(&format!(
                "<tr><td><code>{}</code></td><td>{}</td><td>{:?}</td></tr>",
                esc(&o.case_id),
                o.duration_ms,
                o.status
            ));
        }
        h.push_str("</table>");

        h.push_str(
            "<p class=\"muted\" style=\"margin-top:36px\">由 <code>oz-caselab</code> 生成。\
                    基础设施错误不计为通过；跳过在 release 档视为失败。</p>",
        );
        h.push_str("</div></body></html>");
        h
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Load the accepted signatures (known failures) from a baseline file.
pub fn load_baseline(path: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return out;
    };
    for line in text.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(s) = v.get("signature").and_then(|x| x.as_str()) {
                out.insert(s.to_string());
            }
        }
    }
    out
}
