//! Prompt template engine for the TUI status line and prompts.
//!
//! Inspired by aichat's `left_prompt` / `right_prompt` config:
//!
//! ```toml
//! [tui]
//! left_prompt = "{model} › "
//! right_prompt = "{consume_tokens}"
//! ```
//!
//! Supported variables:
//! - `{model}`        — current model name
//! - `{session}`      — current session name (or "—" if none)
//! - `{agent}`        — current agent name (empty if none; reserved
//!                      for Phase 3 Agent work mode)
//! - `{role}`         — current role (empty if none; reserved for
//!                      Phase 3)
//! - `{rag}`          — current RAG label (empty if none; reserved
//!                      for Phase 1.3)
//! - `{consume_tokens}` — total tokens consumed this turn
//! - `{consume_percent}` — context window usage %
//!
//! Conditional blocks `{?var ...}` and `{!var ...}` are also
//! supported (aichat grammar). A block preceded by `?` renders
//! only if the variable is non-empty; preceded by `!` it renders
//! only if empty. Closing `{}` is required.

use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct Vars {
    pairs: HashMap<String, String>,
}

impl Vars {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.pairs.insert(key.into(), value.into());
        self
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs.get(key).map(String::as_str)
    }

    pub fn is_nonempty(&self, key: &str) -> bool {
        self.pairs.get(key).map(|v| !v.is_empty()).unwrap_or(false)
    }
}

#[derive(Debug, Clone)]
pub struct PromptTemplate {
    raw: String,
}

impl PromptTemplate {
    pub fn new(raw: impl Into<String>) -> Self {
        PromptTemplate { raw: raw.into() }
    }

    pub fn default_left() -> Self {
        PromptTemplate::new("▸ ")
    }

    pub fn default_right() -> Self {
        PromptTemplate::new("")
    }

    pub fn render(&self, vars: &Vars) -> String {
        render_block_inner(&self.raw, vars)
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }
}

fn find_matching_brace(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 1;
    for (offset, &c) in chars.iter().enumerate().skip(open + 1) {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn render_block(inner: &str, vars: &Vars) -> Option<String> {
    let trimmed = inner.trim();
    if let Some(rest) = trimmed.strip_prefix('?') {
        let (name, body) = split_conditional_body(rest);
        return Some(if vars.is_nonempty(name) {
            render_block_inner(body, vars)
        } else {
            String::new()
        });
    }
    if let Some(rest) = trimmed.strip_prefix('!') {
        let (name, body) = split_conditional_body(rest);
        return Some(if !vars.is_nonempty(name) {
            render_block_inner(body, vars)
        } else {
            String::new()
        });
    }
    if let Some(value) = vars.get(trimmed) {
        return Some(value.to_string());
    }
    Some(String::new())
}

fn split_conditional_body(rest: &str) -> (&str, &str) {
    match rest.find(' ') {
        Some(idx) => (&rest[..idx], rest[idx + 1..].trim()),
        None => (rest, ""),
    }
}

fn render_block_inner(src: &str, vars: &Vars) -> String {
    let mut out = String::with_capacity(src.len());
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{' {
            if let Some(end) = find_matching_brace(&chars, i) {
                let inner: String = chars[i + 1..end].iter().collect();
                if let Some(rendered) = render_block(&inner, vars) {
                    out.push_str(&rendered);
                }
                i = end + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(model: &str, session: &str) -> Vars {
        let mut vars = Vars::new();
        vars.insert("model", model);
        vars.insert("session", session);
        vars
    }

    #[test]
    fn plain_text_passthrough() {
        let t = PromptTemplate::new("hello world");
        let out = t.render(&v("m", "s"));
        assert_eq!(out, "hello world");
    }

    #[test]
    fn variable_substitution() {
        let t = PromptTemplate::new("{model} › ");
        let out = t.render(&v("claude", "s"));
        assert_eq!(out, "claude › ");
    }

    #[test]
    fn missing_variable_renders_empty() {
        let t = PromptTemplate::new("[{nonexistent}]");
        let out = t.render(&v("m", "s"));
        assert_eq!(out, "[]");
    }

    #[test]
    fn conditional_true_renders_body() {
        let t = PromptTemplate::new("{?session [{session}]}");
        let out = t.render(&v("m", "my-session"));
        assert_eq!(out, "[my-session]");
    }

    #[test]
    fn conditional_false_renders_empty() {
        let t = PromptTemplate::new("{?session [{session}]}");
        let mut vars = Vars::new();
        vars.insert("model", "m");
        vars.insert("session", "");
        let out = t.render(&vars);
        assert_eq!(out, "");
    }

    #[test]
    fn negation_renders_when_empty() {
        let t = PromptTemplate::new("{!agent default}");
        let mut vars = Vars::new();
        vars.insert("model", "m");
        let out = t.render(&vars);
        assert_eq!(out, "default");
    }

    #[test]
    fn aichat_style_compound_prompt() {
        let t = PromptTemplate::new(
            "{?session {?agent {agent}>}{session}{?role /}}{!session {?agent {agent}>}}{role}{?rag @{rag}} ",
        );
        let mut vars = Vars::new();
        vars.insert("session", "dev");
        vars.insert("role", "coder");
        let out = t.render(&vars);
        assert_eq!(out, "dev/coder ");
    }

    #[test]
    fn nested_braces_in_body() {
        let t = PromptTemplate::new("{?session {session} - extra}");
        let out = t.render(&v("m", "x"));
        assert_eq!(out, "x - extra");
    }

    #[test]
    fn unbalanced_brace_is_literal() {
        let t = PromptTemplate::new("oops { no close");
        let out = t.render(&v("m", "s"));
        assert_eq!(out, "oops { no close");
    }
}
