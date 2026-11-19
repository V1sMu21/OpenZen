//! The six wire properties from the plan — the oracle for G2.
//!
//! These are deliberately weaker than golden values (which would encode the
//! current heuristics) but stronger than "did not panic": they catch the two
//! expensive failure modes — inventing content and losing content.

use serde_json::Value;

/// Recover every string leaf of a JSON value.
pub fn string_leaves(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => {
            if !s.is_empty() {
                out.push(s.clone());
            }
        }
        Value::Array(a) => a.iter().for_each(|x| string_leaves(x, out)),
        Value::Object(m) => m.values().for_each(|x| string_leaves(x, out)),
        _ => {}
    }
}

/// Turn JSON-escaped text back into the form the value would have: enough to
/// compare a recovered value against the raw bytes it came from.
pub fn unescape_jsonish(s: &str) -> String {
    let cs: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < cs.len() {
        if cs[i] != '\\' {
            out.push(cs[i]);
            i += 1;
            continue;
        }
        match cs.get(i + 1).copied() {
            Some('n') => {
                out.push('\n');
                i += 2;
            }
            Some('r') => {
                out.push('\r');
                i += 2;
            }
            Some('t') => {
                out.push('\t');
                i += 2;
            }
            Some('"') => {
                out.push('"');
                i += 2;
            }
            Some('\\') => {
                out.push('\\');
                i += 2;
            }
            Some('u') => {
                let hex: String = cs.iter().skip(i + 2).take(4).collect();
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(c) => {
                        out.push(c);
                        i += 6;
                    }
                    None => {
                        out.push('\\');
                        i += 1;
                    }
                }
            }
            _ => {
                out.push('\\');
                i += 1;
            }
        }
    }
    out
}

/// Property violations for one repair result. An empty vec means pass.
pub fn check_repair(input: &str, out: Option<&Value>) -> Vec<String> {
    let mut errs = Vec::new();
    let Some(v) = out else {
        // Refusing is always allowed; the caller must surface a loud error.
        return errs;
    };
    let Value::Object(map) = v else {
        errs.push(format!("repair returned a non-object: {v}"));
        return errs;
    };
    if map.is_empty() {
        errs.push("repair returned an empty object".into());
    }
    let mut leaves = Vec::new();
    string_leaves(v, &mut leaves);
    // The gateway sometimes double-encodes the arguments object, so a value
    // recovered after one or two JSON unescapes is still "present in the
    // input" — compare against every decoding level, not just the raw bytes.
    let once = unescape_jsonish(input);
    let twice = unescape_jsonish(&once);
    let mut haystacks = vec![input.to_string(), once, twice];
    if let Ok(inner) = serde_json::from_str::<String>(input.trim()) {
        haystacks.push(unescape_jsonish(&inner));
    }
    // A wrapped object (`{"content": "<escaped payload>"}`) is common: collect
    // every decoded string it carries, then decode those one and two more
    // times. Comparing only raw bytes would flag a correctly recovered value
    // as "invented" purely because of encoding layers.
    if let Ok(v) = serde_json::from_str::<Value>(input.trim()) {
        let mut decoded = Vec::new();
        string_leaves(&v, &mut decoded);
        for d in decoded {
            haystacks.push(d.clone());
            haystacks.push(unescape_jsonish(&d));
            haystacks.push(unescape_jsonish(&unescape_jsonish(&d)));
        }
    }
    for leaf in leaves {
        if leaf.len() < 2 {
            continue;
        }
        if haystacks.iter().any(|h| h.contains(&leaf)) {
            continue;
        }
        errs.push(format!(
            "no_invention: value {:?} does not occur in the input ({} bytes)",
            truncate(&leaf, 80),
            input.len()
        ));
        if errs.len() >= 3 {
            break;
        }
    }
    errs
}

/// Property violations for one canonicalization result: idempotent, and no
/// value invented (aliases are moved, never synthesized).
pub fn check_canonicalize(input: &Value, once: &Value, twice: &Value) -> Vec<String> {
    let mut errs = Vec::new();
    if once != twice {
        errs.push("idempotent: canonicalize(canonicalize(x)) != canonicalize(x)".into());
    }
    let mut in_leaves = Vec::new();
    let mut out_leaves = Vec::new();
    string_leaves(input, &mut in_leaves);
    string_leaves(once, &mut out_leaves);
    for leaf in out_leaves {
        if !in_leaves.iter().any(|x| x == &leaf) {
            errs.push(format!(
                "no_invention: {:?} is not in the input",
                truncate(&leaf, 60)
            ));
            if errs.len() >= 3 {
                break;
            }
        }
    }
    errs
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn invention_is_detected() {
        let errs = check_repair(
            "{\"path\": \"/p\"}",
            Some(&json!({"file_path": "/etc/passwd"})),
        );
        assert!(errs.iter().any(|e| e.contains("no_invention")), "{errs:?}");
    }

    #[test]
    fn recovered_value_passes() {
        let input = "{\"file_path\": \"/p/a.txt</parameter>\"}";
        let errs = check_repair(input, Some(&json!({"file_path": "/p/a.txt"})));
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn escaped_newline_is_not_an_invention() {
        let input = r#"{"content": "a\\nb"}"#;
        let errs = check_repair(input, Some(&json!({"content": "a\nb"})));
        assert!(errs.is_empty(), "{errs:?}");
    }

    #[test]
    fn non_idempotent_canonicalize_is_detected() {
        let input = json!({"cmd": "ls"});
        let errs = check_canonicalize(
            &input,
            &json!({"code": "ls", "cmd": "ls"}),
            &json!({"code": "ls"}),
        );
        assert!(errs.iter().any(|e| e.contains("idempotent")), "{errs:?}");
    }
}
