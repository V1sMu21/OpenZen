//! Canonicalization of model-emitted tool arguments.
//!
//! Local / open-weight models frequently emit semantically-correct tool calls
//! with **non-canonical argument key names**: `cmd` / `command` / `script`
//! instead of `code`, `path` / `file` / `filename` instead of `file_path`,
//! `mode: "bash"` where the schema means `type`, or even the tool name itself
//! as the key (`{"code_run": "…"}`). The tools read only their schema's
//! canonical keys, so those calls died with
//! `code_run: missing \`code\` argument` / `write: missing file_path` even
//! though the payload was right there.
//!
//! [`canonicalize_tool_args`] re-keys known aliases onto the canonical name
//! *before* the tool executes. Invariants:
//!
//! - a key the model already supplied correctly is never overwritten;
//! - no value is invented — aliases are only ever moved, never synthesized;
//! - unknown keys are preserved (tools may read optional extras);
//! - the function is idempotent: canonical args pass through unchanged.
//!
//! [`repair_args_against_schema`] is the schema-driven fallback used for tools
//! without a curated table (MCP / dynamically registered tools): it maps the
//! tool-name key, or a lone unrecognized string key, onto the schema's first
//! required property.

use serde_json::Value;

/// Accepted alias keys for one canonical argument.
type Aliases = &'static [&'static str];
/// `(canonical_key, aliases)`.
type Group = (&'static str, Aliases);

/// Normalized key: lowercase, ASCII alphanumerics only. Makes
/// `filePath`, `file_path`, `file-path` and `File Path` equivalent.
fn norm_key(k: &str) -> String {
    k.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn norm_list(items: Aliases) -> Vec<String> {
    items.iter().map(|a| norm_key(a)).collect()
}

// ── code_run ──

const CODE: Group = (
    "code",
    &[
        "cmd",
        "command",
        "commandline",
        "shell",
        "script",
        "bash",
        "sh",
        "code_run",
        "coderun",
        "source",
        "content",
        "input",
        "snippet",
        "program",
        "python",
        "text",
    ],
);
const CODE_TYPE: Group = (
    "type",
    &[
        "mode",
        "lang",
        "language",
        "interpreter",
        "kind",
        "runtime",
        "codelang",
    ],
);
const CODE_TIMEOUT: Group = (
    "timeout",
    &[
        "timeoutsecs",
        "timeoutseconds",
        "maxtime",
        "maxseconds",
        "timeoutvalue",
    ],
);

/// Values that mean "execute this as <language>" when found in a type-ish key.
/// `mode` is included because the wild shape `{"cmd": …, "mode": "bash"}`
/// uses `mode` for the language, while code_run's own `mode` means inline|rpc.
const CODE_LANGUAGES: &[&str] = &[
    "bash",
    "sh",
    "shell",
    "zsh",
    "python",
    "python3",
    "py",
    "node",
    "javascript",
    "js",
    "powershell",
];

/// code_run also has a canonical `mode` (inline|rpc) that shares its name
/// with a common language-key alias, so it is declared here with no aliases
/// to protect it from alias matching.
const CODE_MODE: Group = ("mode", &[]);

const G_CODE_RUN: &[Group] = &[CODE, CODE_TYPE, CODE_TIMEOUT, CODE_MODE];

// ── file tools ──

const FILE: Group = (
    "file_path",
    &[
        "path",
        "file",
        "filename",
        "filepath",
        "filepaths",
        "target",
        "source",
        "src",
        "dest",
        "destination",
        "absolutepath",
        "fullpath",
        "inputfile",
        "fileordir",
        "readpath",
        "writepath",
    ],
);
const CONTENT: Group = (
    "content",
    &[
        "text",
        "data",
        "body",
        "contents",
        "filecontent",
        "newcontent",
        "payload",
        "output",
        "value",
        "code",
        "script",
    ],
);
const OLD: Group = (
    "old_string",
    &[
        "old",
        "oldtext",
        "oldstr",
        "oldcontent",
        "find",
        "search",
        "before",
        "from",
        "original",
        "targettext",
        "oldvalue",
    ],
);
const NEW: Group = (
    "new_string",
    &[
        "new",
        "newtext",
        "newstr",
        "newcontent",
        "replace",
        "replacement",
        "replacementtext",
        "with",
        "after",
        "to",
        "newvalue",
    ],
);
const START: Group = (
    "start",
    &[
        "startline",
        "offset",
        "from",
        "begin",
        "firstline",
        "linestart",
    ],
);
const COUNT: Group = (
    "count",
    &[
        "lines",
        "limit",
        "numlines",
        "linecount",
        "maxlines",
        "length",
    ],
);
const PATTERN: Group = (
    "pattern",
    &[
        "regex",
        "regexp",
        "query",
        "match",
        "globpattern",
        "filepattern",
    ],
);
const DIR: Group = (
    "path",
    &[
        "directory",
        "dir",
        "base",
        "basepath",
        "root",
        "folder",
        "searchpath",
        "cwd",
    ],
);
const INCLUDE: Group = (
    "include",
    &[
        "glob",
        "filter",
        "fileglob",
        "includepattern",
        "include_pattern",
    ],
);

const G_READ: &[Group] = &[FILE, START, COUNT];
const G_WRITE: &[Group] = &[FILE, CONTENT];
const G_EDIT: &[Group] = &[FILE, OLD, NEW];
const G_GLOB: &[Group] = &[PATTERN, DIR];
const G_GREP: &[Group] = &[PATTERN, DIR, INCLUDE];
const G_LS: &[Group] = &[DIR, FILE];

/// Curated alias table. Returns `None` for unknown tools (MCP / dynamic), which
/// are handled by [`repair_args_against_schema`] instead.
fn groups_for(tool: &str) -> Option<&'static [Group]> {
    match norm_key(tool).as_str() {
        "coderun" | "bash" | "shell" | "run" | "execute" | "exec" | "terminal" => Some(G_CODE_RUN),
        "read" | "fileread" | "readfile" | "cat" | "view" => Some(G_READ),
        "write" | "filewrite" | "writefile" | "create" | "save" | "writefilecontent" => {
            Some(G_WRITE)
        }
        "edit" | "patch" | "fileedit" | "editfile" | "replace" | "strreplace" | "applypatch" => {
            Some(G_EDIT)
        }
        "glob" | "listfiles" | "find" => Some(G_GLOB),
        "grep" | "search" | "rg" | "searchfiles" => Some(G_GREP),
        "ls" | "list" | "listdir" | "dir" => Some(G_LS),
        _ => None,
    }
}

/// First (primary) canonical key of a group list — the one whose absence makes
/// the call fail outright.
fn primary_key(groups: &[Group]) -> &'static str {
    groups[0].0
}

/// True when `tool` has a curated alias table (i.e. [`canonicalize_tool_args`]
/// already did everything it can for it).
pub fn has_alias_table(tool: &str) -> bool {
    groups_for(tool).is_some()
}

/// Re-key aliases onto canonical argument names for `tool`.
///
/// Unknown tools pass through unchanged (see [`repair_args_against_schema`]).
pub fn canonicalize_tool_args(tool: &str, args: Value) -> Value {
    let Value::Object(map) = args else {
        return args;
    };
    let Some(groups) = groups_for(tool) else {
        // Not a curated tool (MCP / dynamic): the schema-aware repair in
        // `ToolRegistry::dispatch` handles it — inventing a key name here would
        // mean guessing, and guessing is how a wrong file gets written.
        return Value::Object(map);
    };

    let tool_norm = norm_key(tool);
    let primary = primary_key(groups);
    // Exact canonical parameter names of this tool: always preserved and never
    // treated as an alias of another parameter. Matched EXACTLY, not by
    // normalized form — `filePath` is an alias while `file_path` is canonical,
    // and both normalize to `filepath`.
    let canonical_keys: Vec<&'static str> = groups.iter().map(|(c, _)| *c).collect();

    // (normalized key, exact original key, value). Removal decisions use the
    // exact original key, because a canonical name and an alias can normalize
    // to the same string (`file_path` vs `filePath`).
    let entries: Vec<(String, String, Value)> = map
        .iter()
        .map(|(k, v)| (norm_key(k), k.clone(), v.clone()))
        .collect();

    let mut out = map.clone();
    let mut consumed: Vec<String> = Vec::new();

    // Pass 1 — value-driven: a type-ish key holding a language keyword feeds
    // `type` (the wild shape `{"cmd": …, "mode": "bash", "timeout": 10}`).
    if groups.iter().any(|(c, _)| *c == "type") && !out.contains_key("type") {
        if let Some((_, aliases)) = groups.iter().find(|(c, _)| *c == "type") {
            let mut type_norms = norm_list(aliases);
            type_norms.push("type".to_string());
            let hit = entries
                .iter()
                .find(|(n, orig, v)| {
                    !consumed.contains(orig)
                        && type_norms.contains(n)
                        && v.as_str()
                            .map(|s| {
                                CODE_LANGUAGES.contains(&s.trim().to_ascii_lowercase().as_str())
                            })
                            .unwrap_or(false)
                })
                .cloned();
            if let Some((n, orig, v)) = hit {
                out.insert("type".to_string(), v);
                if n != "type" {
                    consumed.push(orig);
                }
            }
        }
    }

    // Pass 2 — alias → canonical, for every canonical key the model omitted.
    for (canon, aliases) in groups {
        if out.contains_key(*canon) {
            continue;
        }
        let alias_norms = norm_list(aliases);
        let hit = entries
            .iter()
            .find(|(n, orig, _)| {
                !consumed.contains(orig)
                    && !canonical_keys.contains(&orig.as_str())
                    && alias_norms.contains(n)
            })
            .cloned();
        if let Some((_, orig, v)) = hit {
            out.insert((*canon).to_string(), v);
            consumed.push(orig);
        }
    }

    // Pass 3 — the tool's own name used as the primary argument key.
    if !out.contains_key(primary) {
        let hit = entries
            .iter()
            .find(|(n, orig, _)| *n == tool_norm && !consumed.contains(orig))
            .cloned();
        if let Some((_, orig, v)) = hit {
            out.insert(primary.to_string(), v);
            consumed.push(orig);
        }
    }

    // Pass 4 — lone unrecognized string key → primary (last resort).
    if !out.contains_key(primary) {
        let known: Vec<String> = groups
            .iter()
            .flat_map(|(c, a)| {
                let mut v = norm_list(a);
                v.push(norm_key(c));
                v
            })
            .collect();
        let unknown: Vec<(String, String, Value)> = entries
            .iter()
            .filter(|(n, orig, v)| {
                !consumed.contains(orig) && !known.contains(n) && v.is_string() && !n.is_empty()
            })
            .cloned()
            .collect();
        if unknown.len() == 1 {
            let (_, orig, v) = unknown.into_iter().next().unwrap();
            out.insert(primary.to_string(), v);
            consumed.push(orig);
        }
    }

    // Pass 5 — path-family cross-fill: `ls` takes `path` while its siblings
    // take `file_path`, and models mix them freely. Restricted to the two path
    // names on purpose — a `write` that only sent `content` must still fail
    // loudly instead of writing to an argument we guessed.
    if !out.contains_key(primary) && (primary == "path" || primary == "file_path") {
        let other = if primary == "path" { "file_path" } else { "path" };
        let carried = out.get(other).cloned();
        if let Some(v) = carried {
            out.insert(primary.to_string(), v);
        }
    }

    // Drop the keys whose value we moved, plus alias keys that merely duplicate
    // a canonical key already present: leaving `cmd` next to `code` teaches the
    // model, which imitates its own history, to keep sending both.
    let mut drop_norms: Vec<String> = Vec::new();
    for (canon, aliases) in groups {
        if out.contains_key(*canon) {
            drop_norms.extend(norm_list(aliases));
        }
    }
    if out.contains_key(primary) {
        drop_norms.push(tool_norm.clone());
    }
    out.retain(|k, _| {
        if consumed.contains(k) {
            return false; // value was moved onto the canonical key
        }
        if canonical_keys.contains(&k.as_str()) {
            return true; // a real parameter of this tool
        }
        let n = norm_key(k);
        !drop_norms.contains(&n)
    });

    Value::Object(out)
}

/// Schema-driven repair for tools without a curated alias table (MCP /
/// dynamic tools). Maps the tool-name key, or a lone unrecognized string key,
/// onto the schema's first `required` property (falling back to its first
/// property). No-op when the schema gives no usable primary key or the model
/// already supplied one.
pub fn repair_args_against_schema(tool: &str, args: Value, schema: &Value) -> Value {
    let Value::Object(map) = &args else {
        return args;
    };
    let primary = schema
        .get("required")
        .and_then(|r| r.as_array())
        .and_then(|r| r.first())
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            schema
                .get("properties")
                .and_then(|p| p.as_object())
                .and_then(|p| p.keys().next().cloned())
        });
    let Some(primary) = primary else {
        return args;
    };
    let known: Option<Vec<String>> = schema
        .get("properties")
        .and_then(|p| p.as_object())
        .map(|p| p.keys().map(|k| norm_key(k)).collect());
    repair_with_primary(tool, Value::Object(map.clone()), Some(&primary), known.as_deref())
}

/// Shared tail of the repair ladder: tool-name-as-key, then a lone unknown
/// string key, both targeting `primary`.
fn repair_with_primary(
    tool: &str,
    args: Value,
    primary: Option<&str>,
    known: Option<&[String]>,
) -> Value {
    let Value::Object(map) = args else {
        return args;
    };
    // Without a known target key there is nothing to move a value onto.
    let Some(primary) = primary else {
        return Value::Object(map);
    };
    let mut out = map.clone();
    let tool_norm = norm_key(tool);

    if let Some(found) = map.keys().find(|k| norm_key(k) == tool_norm) {
        let v = map[found].clone();
        if !out.contains_key(primary) {
            out.insert(primary.to_string(), v);
            out.remove(found);
            return Value::Object(out);
        }
    }

    if out.contains_key(primary) {
        return Value::Object(out);
    }
    let unknown: Vec<String> = map
        .keys()
        .filter(|k| {
            let n = norm_key(k);
            let is_known = known.map(|ks| ks.contains(&n)).unwrap_or(false)
                || n == norm_key(primary)
                || n == tool_norm;
            !is_known && map[*k].is_string() && !n.is_empty()
        })
        .cloned()
        .collect();
    if unknown.len() == 1 {
        let v = map[&unknown[0]].clone();
        out.insert(primary.to_string(), v);
        out.remove(&unknown[0]);
    }
    Value::Object(out)
}

/// Convenience for logging: a compact `key=value` fingerprint of the argument
/// keys, used to explain a repair in logs and tool errors.
pub fn arg_keys(args: &Value) -> Vec<String> {
    match args {
        Value::Object(m) => m.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// Actionable "missing argument" error.
///
/// A bare `missing \`code\` argument` tells the model nothing about what it
/// actually sent, so weak models retry the same wrong key forever. Naming both
/// the expected key and the received keys lets the next turn self-correct.
pub fn missing_arg_error(tool: &str, missing: &[&str], args: &Value) -> String {
    let sent = arg_keys(args);
    let sent = if sent.is_empty() {
        "(no arguments)".to_string()
    } else {
        sent.join(", ")
    };
    let names = missing
        .iter()
        .map(|m| format!("`{m}`"))
        .collect::<Vec<_>>()
        .join(" and ");
    format!(
        "{tool}: missing {names} argument(s) — this call sent keys [{sent}]. \
         The parameter name(s) are fixed by the tool schema: re-issue the call with {names}."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn code_run_cmd_mode_timeout() {
        let got = canonicalize_tool_args(
            "code_run",
            json!({"cmd": "ls -la", "mode": "bash", "timeout": 10}),
        );
        assert_eq!(got["code"], "ls -la");
        assert_eq!(got["type"], "bash");
        assert_eq!(got["timeout"], 10);
        assert!(got.get("cmd").is_none(), "alias key should be moved: {got}");
        assert!(got.get("mode").is_none(), "type-ified mode should move");
    }

    #[test]
    fn code_run_command_and_type() {
        let got = canonicalize_tool_args(
            "code_run",
            json!({"command": "echo hi", "type": "python", "script": "x"}),
        );
        assert_eq!(got["code"], "echo hi");
        assert_eq!(got["type"], "python");
    }

    #[test]
    fn code_run_tool_name_as_key() {
        let got = canonicalize_tool_args(
            "code_run",
            json!({"code_run": "find /tmp -name '*.rs'", "type": "bash"}),
        );
        assert_eq!(got["code"], "find /tmp -name '*.rs'");
        assert_eq!(got["type"], "bash");
    }

    #[test]
    fn code_run_content_wrapper_shape() {
        let got = canonicalize_tool_args("code_run", json!({"content": "pwd", "type": "bash"}));
        assert_eq!(got["code"], "pwd");
    }

    #[test]
    fn canonical_args_pass_through_unchanged() {
        let args = json!({"code": "ls", "type": "bash", "timeout": 5});
        assert_eq!(canonicalize_tool_args("code_run", args.clone()), args);
    }

    #[test]
    fn canonical_key_always_wins_over_alias() {
        let got = canonicalize_tool_args("code_run", json!({"code": "real", "cmd": "alias"}));
        assert_eq!(got["code"], "real");
        assert_eq!(got.get("cmd"), None);
    }

    #[test]
    fn mode_kept_when_not_a_language() {
        let got = canonicalize_tool_args("code_run", json!({"cmd": "ls", "mode": "rpc"}));
        assert_eq!(got["code"], "ls");
        assert_eq!(got["mode"], "rpc");
        assert!(got.get("type").is_none());
    }

    #[test]
    fn write_path_and_content_aliases() {
        let got = canonicalize_tool_args(
            "write",
            json!({"path": "/tmp/a.txt", "text": "hello", "encoding": "utf-8"}),
        );
        assert_eq!(got["file_path"], "/tmp/a.txt");
        assert_eq!(got["content"], "hello");
        assert_eq!(got["encoding"], "utf-8", "unknown keys preserved");
    }

    #[test]
    fn write_camel_case_path() {
        let got = canonicalize_tool_args("write", json!({"filePath": "a.txt", "data": "x"}));
        assert_eq!(got["file_path"], "a.txt");
        assert_eq!(got["content"], "x");
    }

    #[test]
    fn edit_old_new_aliases() {
        let got = canonicalize_tool_args(
            "edit",
            json!({"file": "a.rs", "old": "foo", "new": "bar"}),
        );
        assert_eq!(got["file_path"], "a.rs");
        assert_eq!(got["old_string"], "foo");
        assert_eq!(got["new_string"], "bar");
    }

    #[test]
    fn read_path_alias_only() {
        let got = canonicalize_tool_args("read", json!({"path": "a.rs", "limit": 20}));
        assert_eq!(got["file_path"], "a.rs");
        assert_eq!(got["count"], 20);
    }

    #[test]
    fn ls_lone_unknown_key() {
        let got = canonicalize_tool_args("ls", json!({"folder_name": "/tmp"}));
        assert_eq!(got["path"], "/tmp");
    }

    // ── shapes taken verbatim from real OpenZen sessions ──

    #[test]
    fn real_shape_cmd_mode_timeout() {
        // `~/.openzen/openzen/sessions.json` — the most frequent failure.
        let got = canonicalize_tool_args(
            "code_run",
            json!({
                "cmd": "cd /tmp/finance-dashboard && source .venv/bin/activate && pip list",
                "mode": "bash",
                "timeout": 10
            }),
        );
        assert!(got["code"].is_string());
        assert_eq!(got["type"], "bash");
    }

    #[test]
    fn real_shape_command_mode_type() {
        let got =
            canonicalize_tool_args("code_run", json!({"command": "ls", "mode": "shell", "type": "bash"}));
        assert_eq!(got["code"], "ls");
        assert_eq!(got["type"], "bash");
    }

    #[test]
    fn real_shape_cmd_type() {
        let got = canonicalize_tool_args("code_run", json!({"cmd": "pwd", "type": "bash"}));
        assert_eq!(got["code"], "pwd");
    }

    #[test]
    fn real_shape_content_type() {
        let got = canonicalize_tool_args("code_run", json!({"content": "pwd", "type": "bash"}));
        assert_eq!(got["code"], "pwd");
    }

    #[test]
    fn real_shape_tool_name_key() {
        let got = canonicalize_tool_args(
            "code_run",
            json!({"code_run": "SP=/Applications/oMLX-bonsai.app; find $SP -name '*.log'", "type": "bash"}),
        );
        assert!(got["code"].as_str().unwrap().contains("oMLX-bonsai"));
    }

    #[test]
    fn canonicalization_is_idempotent() {
        // It runs twice on the same call: once when the stream parses the tool
        // call, once again at dispatch.
        for (tool, args) in [
            ("code_run", json!({"cmd": "ls", "mode": "bash", "timeout": 3})),
            ("write", json!({"path": "a.txt", "text": "body"})),
            ("edit", json!({"file": "a.rs", "old": "x", "new": "y"})),
            ("read", json!({"path": "a.rs", "limit": 5})),
            ("code_run", json!({"code_run": "ls", "type": "bash"})),
            ("ls", json!({"folder_name": "/tmp"})),
        ] {
            let once = canonicalize_tool_args(tool, args.clone());
            let twice = canonicalize_tool_args(tool, once.clone());
            assert_eq!(once, twice, "{tool} not idempotent for {args}");
        }
    }

    #[test]
    fn missing_arg_error_names_expected_and_sent_keys() {
        let msg = missing_arg_error("code_run", &["code"], &json!({"cmd": "ls"}));
        assert!(msg.contains("`code`"), "{msg}");
        assert!(msg.contains("cmd"), "{msg}");
    }

    #[test]
    fn ls_accepts_file_path() {
        let got = canonicalize_tool_args("ls", json!({"file_path": "/tmp"}));
        assert_eq!(got["path"], "/tmp");
        // …but a non-path argument must never be promoted to the primary key.
        let got = canonicalize_tool_args("write", json!({"content": "x"}));
        assert!(got.get("file_path").is_none(), "{got}");
    }

    #[test]
    fn unknown_tool_passes_through() {
        let args = json!({"whatever": 1});
        assert_eq!(canonicalize_tool_args("mcp__foo__bar", args.clone()), args);
    }

    #[test]
    fn schema_repair_maps_tool_name_key() {
        let schema = json!({
            "type": "object",
            "properties": {"query": {"type": "string"}},
            "required": ["query"]
        });
        let got = repair_args_against_schema("mcp__x__search", json!({"mcp__x__search": "hi"}), &schema);
        assert_eq!(got["query"], "hi");
    }

    #[test]
    fn schema_repair_maps_lone_string_key() {
        let schema = json!({
            "type": "object",
            "properties": {"query": {"type": "string"}, "limit": {"type": "integer"}},
            "required": ["query"]
        });
        let got = repair_args_against_schema("mcp__x__search", json!({"q": "hi"}), &schema);
        assert_eq!(got["query"], "hi");
    }

    #[test]
    fn schema_repair_does_not_overwrite() {
        let schema = json!({
            "type": "object",
            "properties": {"query": {"type": "string"}},
            "required": ["query"]
        });
        let got = repair_args_against_schema("mcp__x__search", json!({"query": "keep"}), &schema);
        assert_eq!(got["query"], "keep");
    }
}
