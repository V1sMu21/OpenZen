//! Delivery-quality pipeline (P2-10): spec anchor + `[verify]` assertions +
//! independent multi-perspective review.
//!
//! The agent loop calls into this module at exit time when `quality_gates`
//! is enabled:
//!
//! 1. `read_spec` / `load_assertions` — parse `[verify] <shell cmd>` lines
//!    from the task spec file the agent wrote at task start (spec-first).
//! 2. `run_assertion_gate` — execute each assertion in `working_dir`;
//!    failures are fed back as next-turn prompts (bounded by
//!    `assertion_max_rounds` in the loop).
//! 3. `run_independent_review` — one clean-context LLM call that reviews
//!    the spec + deliverable list from multiple perspectives (spec-fit,
//!    completeness, runnability, quality). Issues feed back once.
//!
//! Fail-open by design: any error/timeout here must never block the loop.
//! Quality gates improve delivery; they do not hang it.

use std::path::Path;
use std::time::Duration;

use oz_core_types::LlmClient;

/// Fixed spec filename in the agent working directory (visible to the user
/// as a deliverable itself).
pub const SPEC_FILE: &str = "task_spec.md";

const MAX_ASSERTIONS: usize = 8;
const ASSERTION_TIMEOUT_SECS: u64 = 60;
const ASSERTION_OUTPUT_LIMIT: usize = 4096;
const REVIEW_TIMEOUT_SECS: u64 = 90;

/// One failed `[verify]` assertion: the command and its captured output.
#[derive(Debug, Clone)]
pub struct AssertionFailure {
    pub command: String,
    pub output: String,
}

/// A review issue with severity (high / medium / low).
#[derive(Debug, Clone)]
pub struct ReviewIssue {
    pub severity: String,
    pub item: String,
}

/// Verdict of the independent review.
#[derive(Debug, Clone)]
pub struct ReviewVerdict {
    pub pass: bool,
    pub issues: Vec<ReviewIssue>,
}

impl Default for ReviewVerdict {
    fn default() -> Self {
        ReviewVerdict {
            pass: true,
            issues: Vec::new(),
        }
    }
}

/// Read the task spec file from `working_dir` if present.
pub fn read_spec(working_dir: &str) -> Option<String> {
    let path = Path::new(working_dir).join(SPEC_FILE);
    std::fs::read_to_string(&path).ok()
}

/// Parse `[verify] <command>` lines from the spec text.
///
/// Format the agent is instructed to use in sys_prompt:
///   ## 验收标准
///   [verify] python3 scripts/check_assets.py
///   [verify] cargo test --lib
///
/// Lines must be non-empty and contain a single command after the marker.
/// The command is taken verbatim (may contain spaces / pipes) and executed
/// via `sh -c` in the working directory.
pub fn load_assertions(spec_text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in spec_text.lines() {
        let trimmed = line.trim();
        let cmd = trimmed
            .strip_prefix("[verify]")
            .or_else(|| trimmed.strip_prefix("[VERIFY]"))
            .map(|s| s.trim());
        if let Some(cmd) = cmd {
            if !cmd.is_empty() && out.len() < MAX_ASSERTIONS {
                out.push(cmd.to_string());
            }
        }
    }
    out
}

/// Execute a single assertion command in `working_dir`.
/// Returns (passed, output) with bounded timeout and truncated output.
pub async fn run_assertion(command: &str, working_dir: &str) -> (bool, String) {
    let dir = std::path::PathBuf::from(working_dir);
    let cmd = command.to_string();
    let out = tokio::task::spawn_blocking(move || {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .current_dir(&dir)
            .output()
    })
    .await;

    let (status, stdout, stderr) = match out {
        Ok(Ok(o)) => (o.status, o.stdout, o.stderr),
        _ => return (false, "<verify failed to spawn>".to_string()),
    };
    let mut buf = stdout;
    buf.extend_from_slice(&stderr);
    let text = String::from_utf8_lossy(&buf).trim().to_string();
    let text = truncate(&text, ASSERTION_OUTPUT_LIMIT);
    (status.success(), text)
}

/// Run all assertions parsed from `spec_text`. Returns the list of failures
/// (empty = all passed or no assertions present).
pub async fn run_assertion_gate(spec_text: &str, working_dir: &str) -> Vec<AssertionFailure> {
    let assertions = load_assertions(spec_text);
    if assertions.is_empty() {
        return Vec::new();
    }
    let mut failures = Vec::new();
    for cmd in &assertions {
        let (passed, output) = tokio::time::timeout(
            Duration::from_secs(ASSERTION_TIMEOUT_SECS),
            run_assertion(cmd, working_dir),
        )
        .await
        .unwrap_or_else(|_| {
            (
                false,
                format!("<verify timed out after {ASSERTION_TIMEOUT_SECS}s>"),
            )
        });
        if !passed {
            failures.push(AssertionFailure {
                command: cmd.clone(),
                output,
            });
        }
    }
    failures
}

/// Build the independent-review prompt. The reviewer gets a CLEAN context:
/// only the spec, the deliverable list and the final reply — never the
/// implementation transcript (Cognition: clean context improves review).
pub fn build_review_prompt(
    spec: &str,
    deliverables: &[String],
    final_reply: &str,
    lang: &str,
) -> String {
    let deliverables = if deliverables.is_empty() {
        "(none listed)".to_string()
    } else {
        deliverables.join("\n")
    };
    let final_reply = if final_reply.trim().is_empty() {
        "(no final reply)".to_string()
    } else {
        final_reply.to_string()
    };
    let zh = lang == "zh";
    let instruction = if zh {
        "你是独立的交付质量评审员。对照任务规格，从以下视角审查交付物：\
         \n1. 规格符合度：交付物是否覆盖规格中的每项目标？\
         \n2. 完整性：规格中列出的交付物是否齐全？\
         \n3. 运行可行性：交付物能否实际运行/使用（代码能否编译、资源引用是否完整）？\
         \n4. 质量问题：明显缺陷、缺失、粗糙之处。\
         \n5. 交叉一致性：方向/比例/布局与玩法是否自洽（竖版卷轴→角色朝上）；\
         动画/滚动/计时类行为（如残影、闪烁）是否经过长时间运行验证（≥60 秒）而非单帧截图。\
         \n只输出一个 JSON 对象（不要 markdown 代码块、不要其他文字）：\
         \n{\"pass\": true 或 false, \"issues\": [{\"severity\": \"high|medium|low\", \"item\": \"具体问题\"}]}\
         \npass 为 false 仅当存在必须修复的 high 级问题；low/medium 问题在 issues 中列出但 pass 仍可为 true。"
    } else {
        "You are an independent delivery quality reviewer. Review the deliverables \
         against the task spec from these perspectives:\n\
         1. Spec fit: does each goal in the spec get delivered?\n\
         2. Completeness: are all listed deliverables present?\n\
         3. Runnable: will the deliverable actually work (code compiles, assets referenced exist)?\n\
         4. Quality: obvious defects, gaps, rough edges.\n\
         5. Cross-consistency: are orientation/scale/layout consistent with the \
         gameplay (vertical scroller -> ship faces up)? Have time-dependent \
         behaviors (ghosting, flicker) been validated with a long run (>=60s) \
         rather than a single-frame screenshot?\n\
         Output ONLY a JSON object (no markdown fences, no other text):\n\
         {\"pass\": true or false, \"issues\": [{\"severity\": \"high|medium|low\", \"item\": \"specific problem\"}]}\n\
         pass is false only when there is a high-severity issue that must be fixed; \
         low/medium issues go in issues but pass may stay true."
    };

    format!(
        "{}\n\n## 任务规格 (task_spec.md)\n{}\n\n## 交付物清单\n{}\n\n## 候选最终回复\n{}",
        instruction, spec, deliverables, final_reply
    )
}

/// Tolerantly parse the review JSON response (strips markdown fences and
/// surrounding prose). Fail-open: unparseable input → pass=true.
pub fn parse_review_response(raw: &str) -> ReviewVerdict {
    let raw = raw.trim();
    let body = strip_markdown_fence(raw);
    let start = body.find('{');
    let end = body.rfind('}');
    let json_text = match (start, end) {
        (Some(s), Some(e)) if e > s => &body[s..=e],
        _ => {
            tracing::warn!("[quality] review response not JSON: {}", truncate(raw, 200));
            return ReviewVerdict::default();
        }
    };
    match serde_json::from_str::<serde_json::Value>(json_text) {
        Ok(v) => {
            let pass = v.get("pass").and_then(|p| p.as_bool()).unwrap_or(true);
            let issues = v
                .get("issues")
                .and_then(|i| i.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|it| {
                            let severity = it
                                .get("severity")
                                .and_then(|s| s.as_str())
                                .unwrap_or("low")
                                .to_string();
                            let item = it
                                .get("item")
                                .and_then(|s| s.as_str())
                                .unwrap_or("")
                                .to_string();
                            if item.is_empty() {
                                None
                            } else {
                                Some(ReviewIssue { severity, item })
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if !pass && issues.is_empty() {
                return ReviewVerdict {
                    pass: false,
                    issues: vec![ReviewIssue {
                        severity: "high".into(),
                        item: "评审未通过，但未提供具体问题清单".into(),
                    }],
                };
            }
            ReviewVerdict { pass, issues }
        }
        Err(e) => {
            tracing::warn!("[quality] review JSON parse failed: {e}");
            ReviewVerdict::default()
        }
    }
}

/// Run the independent review with one clean-context LLM call.
/// Returns None on transport failure (fail-open — caller proceeds).
pub async fn run_independent_review<C: LlmClient>(
    client: &mut C,
    spec: &str,
    deliverables: &[String],
    final_reply: &str,
    lang: &str,
) -> Option<ReviewVerdict> {
    let prompt = build_review_prompt(spec, deliverables, final_reply, lang);
    let msg = oz_core_types::Message::user(prompt);
    match tokio::time::timeout(
        Duration::from_secs(REVIEW_TIMEOUT_SECS),
        client.chat(&[msg], &[]),
    )
    .await
    {
        Ok(Ok(resp)) => {
            let verdict = parse_review_response(&resp.content);
            tracing::info!(
                "[quality] independent review: pass={} issues={}",
                verdict.pass,
                verdict.issues.len()
            );
            Some(verdict)
        }
        Ok(Err(e)) => {
            tracing::warn!("[quality] review LLM call failed (fail-open): {e}");
            None
        }
        Err(_) => {
            tracing::warn!("[quality] review timed out (fail-open)");
            None
        }
    }
}

/// Hint injected when the agent did file writes without creating a spec
/// first (spec anchor, one-shot guidance, no loop).
pub fn spec_anchor_hint(lang: &str) -> String {
    if lang == "zh" {
        "[SPEC] 你已进行文件写入，但工作区还没有 task_spec.md。\
         请先创建任务规格 task_spec.md（任务目标 / 交付物清单 / 验收标准），\
         验收标准用可执行断言格式，每行一条：\n[verify] <shell 命令>\n\
         例如：\n[verify] python3 scripts/check_assets.py\n[verify] cargo test --lib\n\
         游戏/动态类任务额外建议：\n\
         [verify] python3 scripts/run_sim.py --duration 60（长运行验证，覆盖残影/闪烁）\n\
         [verify] python3 scripts/check_assets.py --pixels（素材透明通道/尺寸/朝向检查）\n\
         之后继续执行任务。规格文件会成为交付前验收的依据。"
            .to_string()
    } else {
        "[SPEC] You have written files but no task_spec.md exists in the \
         working directory. Create the task spec task_spec.md first (goals / \
         deliverable list / acceptance criteria). Write acceptance criteria \
         as executable assertions, one per line:\n[verify] <shell command>\n\
         e.g.:\n[verify] python3 scripts/check_assets.py\n[verify] cargo test --lib\n\
         For game/dynamic tasks, additionally:\n\
         [verify] python3 scripts/run_sim.py --duration 60 (long-run validation)\n\
         [verify] python3 scripts/check_assets.py --pixels (asset alpha/size/orientation)\n\
         Then continue. This spec file is what acceptance will check against."
            .to_string()
    }
}

/// Collect the file paths written during the run (deliverables list).
pub fn collect_deliverables(tool_sequence: &[(String, serde_json::Value)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (name, args) in tool_sequence {
        if !matches!(
            name.as_str(),
            "write" | "file_write" | "edit" | "file_edit" | "patch" | "file_patch"
        ) {
            continue;
        }
        if let Some(path) = args.get("file_path").and_then(|v| v.as_str()) {
            if !path.is_empty() && !out.iter().any(|p| p == path) {
                out.push(path.to_string());
            }
        }
    }
    out
}

/// True when the run performed any file-writing tool call.
pub fn has_write_operations(tool_sequence: &[(String, serde_json::Value)]) -> bool {
    tool_sequence.iter().any(|(name, _)| {
        matches!(
            name.as_str(),
            "write" | "file_write" | "edit" | "file_edit" | "patch" | "file_patch"
        )
    })
}

// ── In-turn quick verification (P2-10) ───────────────────────────────────
// "Environment as ground truth": after a write/edit turn, run a fast check
// (cargo check for Rust workspaces) and feed failures back immediately —
// instead of only verifying at exit time.

/// Run a quick check after a write/edit turn. Returns a feedback prompt on
/// failure (None when nothing to check or everything passes).
pub async fn quick_verify_after_write(
    tool_names: &[String],
    working_dir: &str,
    lang: &str,
) -> Option<String> {
    let has_write = tool_names.iter().any(|n| {
        matches!(
            n.as_str(),
            "write" | "file_write" | "edit" | "file_edit" | "patch" | "file_patch"
        )
    });
    if !has_write {
        return None;
    }

    // Rust workspace → cargo check (quiet, bounded).
    if Path::new(working_dir).join("Cargo.toml").is_file() {
        let (passed, output) = run_quick_cmd("cargo check --quiet", working_dir, 45).await;
        if !passed {
            return Some(quick_check_feedback("cargo check --quiet", &output, lang));
        }
    }
    None
}

async fn run_quick_cmd(cmd: &str, working_dir: &str, timeout_secs: u64) -> (bool, String) {
    let dir = std::path::PathBuf::from(working_dir);
    let cmd = cmd.to_string();
    let fut = tokio::task::spawn_blocking(move || {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .current_dir(&dir)
            .output()
    });
    let out = match tokio::time::timeout(Duration::from_secs(timeout_secs.max(1)), fut).await {
        Ok(Ok(Ok(o))) => o,
        _ => {
            return (
                false,
                format!("<quick check timed out after {timeout_secs}s>"),
            )
        }
    };
    let (status, stdout, stderr) = (out.status, out.stdout, out.stderr);
    let mut buf = stdout;
    buf.extend_from_slice(&stderr);
    let text = String::from_utf8_lossy(&buf).trim().to_string();
    (status.success(), truncate(&text, 2000))
}

fn quick_check_feedback(cmd: &str, output: &str, lang: &str) -> String {
    if lang == "zh" {
        format!("[CHECK] 自动检查未通过：`{cmd}`\n{output}\n请根据报错修复后继续。")
    } else {
        format!(
            "[CHECK] Automatic check failed: `{cmd}`\n{output}\nFix from the errors and continue."
        )
    }
}

// ── Failure reflection log (Reflexion-style, P2-10) ──────────────────────
// Failures are written to {working_dir}/.openzen/reflections.jsonl so later
// tasks can read prior mistakes instead of repeating them.

/// Append a failure reflection entry (JSONL). Never fails the loop.
pub fn log_reflection(working_dir: &str, failure_type: &str, summary: &str) {
    let dir = Path::new(working_dir).join(".openzen");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let entry = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "type": failure_type,
        "summary": truncate(summary, 600),
    });
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("reflections.jsonl"))
    {
        let _ = writeln!(f, "{}", entry);
    }
}

/// True when a prior reflection log exists (the reminder points to it).
pub fn reflection_log_exists(working_dir: &str) -> bool {
    Path::new(working_dir)
        .join(".openzen")
        .join("reflections.jsonl")
        .is_file()
}

// ── Unresolved-suspicion closure (P2) ────────────────────────────────────
// The OpenZen task-1 post-mortem found the agent noticed "ship may face
// left" but never followed up (loop never enforced closure). This scan
// surfaces recent uncertainty phrases at exit time so the agent must
// confirm/fix them before responding.

/// Only suspicions from the last N assistant messages count (recency filter).
const SUSPICION_RECENT_LIMIT: usize = 3;
/// Fix/verify tools that count as "dealing with" a suspicion.
const FIX_TOOLS: [&str; 7] = [
    "write",
    "file_write",
    "edit",
    "file_edit",
    "patch",
    "file_patch",
    "code_run",
];

const SUSPICION_ZH: [&str; 11] = [
    "可能",
    "疑似",
    "似乎",
    "好像",
    "看起来",
    "怀疑",
    "不确定",
    "待检查",
    "待验证",
    "之后再",
    "回头再",
];
const SUSPICION_EN: [&str; 10] = [
    "maybe",
    "might",
    "possibly",
    "seems",
    "appears",
    "suspect",
    "uncertain",
    "not sure",
    "looks off",
    "check later",
];

/// Detect an unresolved suspicion in the most recent assistant messages.
///
/// - Recency: only the last [`SUSPICION_RECENT_LIMIT`] assistant messages.
/// - Fix-activity filter: if the most recent tools include a write/edit/
///   patch/code_run, the agent is presumably already handling it.
/// - Returns a closure prompt quoting the suspicion (one-shot; the loop
///   lets the agent confirm or fix, then respond again).
pub fn find_unresolved_suspicion(
    messages: &[oz_core_types::Message],
    tool_sequence: &[(String, serde_json::Value)],
    lang: &str,
) -> Option<String> {
    let assistant: Vec<String> = messages
        .iter()
        .filter(|m| m.role == oz_core_types::Role::Assistant)
        .filter_map(|m| {
            let text: Vec<&str> = m
                .content
                .iter()
                .filter_map(|b| match b {
                    oz_core_types::ContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            if text.is_empty() {
                None
            } else {
                Some(text.join(" "))
            }
        })
        .collect();
    let recent: Vec<&String> = assistant
        .iter()
        .rev()
        .take(SUSPICION_RECENT_LIMIT)
        .collect();
    if recent.is_empty() {
        return None;
    }

    // Fix-activity filter: recent fix/verify actions mean the suspicion is
    // presumably being dealt with (or has been).
    let last_tools: Vec<&str> = tool_sequence
        .iter()
        .rev()
        .take(5)
        .map(|(n, _)| n.as_str())
        .collect();
    if last_tools.iter().any(|t| FIX_TOOLS.contains(t)) {
        return None;
    }

    let keys: &[&str] = if lang == "zh" {
        &SUSPICION_ZH
    } else {
        &SUSPICION_EN
    };
    for msg in recent {
        for sentence in split_sentences(msg) {
            let lower = sentence.to_lowercase();
            if keys.iter().any(|k| lower.contains(&k.to_lowercase())) {
                let hint = if lang == "zh" {
                    format!(
                        "[SUSPICION] 你最近提到过可疑点：「{}」。\
                         该疑点是否已查证/解决？未解决请处理后再 respond 退出；\
                         已解决或无需处理请简要确认后继续。",
                        truncate(sentence, 200)
                    )
                } else {
                    format!(
                        "[SUSPICION] You recently noted something questionable: \"{}\". \
                         Has it been investigated/resolved? If not, fix it before responding; \
                         if already handled or not an issue, briefly confirm and continue.",
                        truncate(sentence, 200)
                    )
                };
                return Some(hint);
            }
        }
    }
    None
}

/// Split text into sentences (Chinese/English punctuation + newlines).
fn split_sentences(text: &str) -> Vec<&str> {
    text.split(['。', '！', '？', '；', '\n', '.', '!', '?', ';'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && s.chars().count() > 3)
        .collect()
}

/// Format assertion failures as a next-turn feedback prompt (bounded size).
pub fn format_assertion_feedback(failures: &[AssertionFailure], lang: &str, round: u32) -> String {
    let mut out = String::new();
    if lang == "zh" {
        out.push_str(&format!(
            "[VERIFY] {} 条验收断言未通过（第 {} 轮修复）：\n",
            failures.len(),
            round
        ));
    } else {
        out.push_str(&format!(
            "[VERIFY] {} acceptance assertion(s) failed (fix round {}):\n",
            failures.len(),
            round
        ));
    }
    for f in failures {
        out.push_str(&format!("$ {}\n{}\n", f.command, truncate(&f.output, 1500)));
    }
    out.push_str(if lang == "zh" {
        "修复问题后再次调用 respond 退出。"
    } else {
        "Fix the issues and call respond again."
    });
    out
}

/// Format review issues (high severity only) as a next-turn feedback prompt.
pub fn format_review_feedback(issues: &[ReviewIssue], lang: &str) -> String {
    let mut out = String::new();
    out.push_str(if lang == "zh" {
        "[REVIEW] 独立评审发现以下必须修复的问题：\n"
    } else {
        "[REVIEW] Independent review found issues that must be fixed:\n"
    });
    for issue in issues.iter().filter(|i| i.severity == "high") {
        out.push_str(&format!(
            "- [{}] {}\n",
            issue.severity,
            truncate(&issue.item, 500)
        ));
    }
    out.push_str(if lang == "zh" {
        "修复后再次调用 respond 退出。"
    } else {
        "Fix them and call respond again."
    });
    out
}

fn strip_markdown_fence(s: &str) -> &str {
    let s = s.trim();
    s.strip_prefix("```json")
        .unwrap_or(s)
        .strip_prefix("```JSON")
        .unwrap_or(s)
        .strip_prefix("```")
        .unwrap_or(s)
        .strip_suffix("```")
        .unwrap_or(s)
        .trim()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push_str("\n… [truncated]");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_assertions_parses_verify_lines() {
        let spec = "\
# 任务规格
## 验收标准
[verify] python3 scripts/check_assets.py
[verify] cargo test --lib
[verify] test -f assets/logo.png && echo ok
普通文本行不是断言
";
        let cmds = load_assertions(spec);
        assert_eq!(cmds.len(), 3);
        assert_eq!(cmds[0], "python3 scripts/check_assets.py");
        assert_eq!(cmds[1], "cargo test --lib");
        assert_eq!(cmds[2], "test -f assets/logo.png && echo ok");
    }

    #[test]
    fn test_load_assertions_caps_and_ignores_garbage() {
        let spec = "[verify] cmd1\n[verify]\n[verify]   \n[verify] cmd2\n";
        let cmds = load_assertions(spec);
        assert_eq!(cmds, vec!["cmd1".to_string(), "cmd2".to_string()]);
    }

    #[test]
    fn test_load_assertions_empty_spec() {
        assert!(load_assertions("").is_empty());
        assert!(load_assertions("no markers here").is_empty());
    }

    #[tokio::test]
    async fn test_run_assertion_pass_and_fail() {
        let (p, _out) = run_assertion("true", "/tmp").await;
        assert!(p);
        let (p2, _) = run_assertion("false", "/tmp").await;
        assert!(!p2);
        // output captured
        let (p3, out3) = run_assertion("echo hello-assert", "/tmp").await;
        assert!(p3);
        assert!(out3.contains("hello-assert"));
    }

    #[tokio::test]
    async fn test_assertion_gate_reports_only_failures() {
        let spec = "[verify] true\n[verify] false\n[verify] echo ok\n";
        let failures = run_assertion_gate(spec, "/tmp").await;
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].command, "false");
    }

    #[tokio::test]
    async fn test_assertion_gate_no_assertions() {
        let failures = run_assertion_gate("no verify lines", "/tmp").await;
        assert!(failures.is_empty());
    }

    #[test]
    fn test_parse_review_json_plain() {
        let raw = r#"{"pass": false, "issues": [{"severity": "high", "item": "资源引用缺失"}]}"#;
        let v = parse_review_response(raw);
        assert!(!v.pass);
        assert_eq!(v.issues.len(), 1);
        assert_eq!(v.issues[0].severity, "high");
    }

    #[test]
    fn test_parse_review_json_in_fence() {
        let raw = "评审结果：\n```json\n{\"pass\": true, \"issues\": [{\"severity\": \"low\", \"item\": \"命名可优化\"}]}\n```\n结束";
        let v = parse_review_response(raw);
        assert!(v.pass);
        assert_eq!(v.issues.len(), 1);
    }

    #[test]
    fn test_parse_review_fail_open_on_garbage() {
        let v = parse_review_response("我无法评审这个任务");
        assert!(v.pass, "unparseable review must fail open");
        assert!(v.issues.is_empty());
    }

    #[test]
    fn test_parse_review_fail_without_issues_gets_placeholder() {
        let v = parse_review_response(r#"{"pass": false}"#);
        assert!(!v.pass);
        assert_eq!(v.issues.len(), 1);
    }

    #[test]
    fn test_collect_deliverables() {
        let seq: Vec<(String, serde_json::Value)> = vec![
            (
                "write".into(),
                serde_json::json!({"file_path": "src/main.rs"}),
            ),
            (
                "edit".into(),
                serde_json::json!({"file_path": "src/main.rs"}),
            ),
            ("code_run".into(), serde_json::json!({"code": "ls"})),
            (
                "file_write".into(),
                serde_json::json!({"file_path": "assets/logo.png"}),
            ),
        ];
        let dels = collect_deliverables(&seq);
        assert_eq!(
            dels,
            vec!["src/main.rs".to_string(), "assets/logo.png".to_string()]
        );
        assert!(has_write_operations(&seq));
    }

    #[test]
    fn test_has_write_operations_false_for_reads() {
        let seq: Vec<(String, serde_json::Value)> = vec![
            ("read".into(), serde_json::json!({"file_path": "a.txt"})),
            ("grep".into(), serde_json::json!({"pattern": "x"})),
        ];
        assert!(!has_write_operations(&seq));
    }

    #[test]
    fn test_spec_anchor_hint_mentions_verify() {
        assert!(spec_anchor_hint("zh").contains("[verify]"));
        assert!(spec_anchor_hint("en").contains("[verify]"));
    }

    // ── find_unresolved_suspicion tests ──

    fn msg(role: &str, text: &str) -> oz_core_types::Message {
        let mut m = oz_core_types::Message::user(text);
        if role == "assistant" {
            m.role = oz_core_types::Role::Assistant;
        }
        m
    }

    #[test]
    fn test_suspicion_detected_no_fix_activity() {
        let messages = vec![
            msg("assistant", "已完成游戏主逻辑。"),
            msg("assistant", "飞船的朝向可能有问题，素材看起来朝左。"),
        ];
        let seq: Vec<(String, serde_json::Value)> = vec![
            ("read".into(), serde_json::json!({})),
            ("grep".into(), serde_json::json!({})),
        ];
        let hint = find_unresolved_suspicion(&messages, &seq, "zh");
        assert!(
            hint.is_some(),
            "suspicion without fix activity must be flagged"
        );
        assert!(hint.unwrap().contains("朝向可能有问题"));
    }

    #[test]
    fn test_suspicion_suppressed_when_recent_fix() {
        let messages = vec![msg("assistant", "飞船的朝向可能有问题，我重新生成素材。")];
        let seq: Vec<(String, serde_json::Value)> =
            vec![("write".into(), serde_json::json!({"file_path": "ship.png"}))];
        assert!(
            find_unresolved_suspicion(&messages, &seq, "zh").is_none(),
            "recent write action means the suspicion is being handled"
        );
    }

    #[test]
    fn test_suspicion_ignored_when_no_keywords() {
        let messages = vec![msg("assistant", "游戏已完成，所有功能测试通过。")];
        let seq: Vec<(String, serde_json::Value)> = vec![];
        assert!(find_unresolved_suspicion(&messages, &seq, "zh").is_none());
    }

    #[test]
    fn test_suspicion_ignored_when_stale() {
        // 4 assistant messages; the suspicion is 4th-from-last → beyond the
        // recency window of 3. No fix activity, so only recency suppresses it.
        let messages = vec![
            msg("assistant", "飞船朝向可能有问题。"),
            msg("assistant", "修复了分数显示。"),
            msg("assistant", "修复了暂停逻辑。"),
            msg("assistant", "修复了碰撞检测。"),
        ];
        let seq: Vec<(String, serde_json::Value)> = vec![("read".into(), serde_json::json!({}))];
        assert!(
            find_unresolved_suspicion(&messages, &seq, "zh").is_none(),
            "suspicion older than the recency window must not be flagged"
        );
    }

    #[test]
    fn test_suspicion_english_keywords() {
        let messages = vec![msg("assistant", "The ship sprite seems to face left.")];
        let seq: Vec<(String, serde_json::Value)> = vec![("grep".into(), serde_json::json!({}))];
        let hint = find_unresolved_suspicion(&messages, &seq, "en");
        assert!(hint.is_some());
        assert!(hint.unwrap().contains("seems to face left"));
    }

    #[test]
    fn test_suspicion_empty_messages() {
        let messages: Vec<oz_core_types::Message> = vec![];
        let seq: Vec<(String, serde_json::Value)> = vec![];
        assert!(find_unresolved_suspicion(&messages, &seq, "zh").is_none());
    }

    #[test]
    fn test_review_prompt_includes_cross_consistency() {
        let zh_prompt = build_review_prompt("spec", &["a.js".to_string()], "done", "zh");
        assert!(zh_prompt.contains("交叉一致性"));
        let en_prompt = build_review_prompt("spec", &["a.js".to_string()], "done", "en");
        assert!(en_prompt.contains("Cross-consistency"));
    }
}
