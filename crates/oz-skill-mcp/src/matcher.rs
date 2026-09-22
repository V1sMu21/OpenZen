//! Unified matching engine — cross-type knowledge search.
//!
//! Provides a single entry point for finding relevant knowledge
//! (skills, SOPs) given a user query. Used by the agent loop at
//! turn start to inject relevant context into the system prompt.

use std::collections::HashMap;

use oz_core_types::SkillMcpType;

use crate::skill::Skill;
use crate::sop::Sop;

/// A match result with score and type.
#[derive(Debug, Clone)]
pub struct MatchItem {
    /// Knowledge type.
    pub kind: SkillMcpType,
    /// Display name.
    pub name: String,
    /// Content suitable for prompt injection.
    pub content: String,
    /// Relevance score (0.0–1.0).
    pub score: f32,
    /// Whether this is a skill (true) or SOP (false).
    pub is_skill: bool,
}

/// Match configuration.
#[derive(Debug, Clone)]
pub struct MatchConfig {
    /// Maximum number of skills to include.
    pub max_skills: usize,
    /// Maximum number of SOPs to include.
    pub max_sops: usize,
    /// Minimum score threshold for inclusion.
    pub min_score: f32,
}

impl Default for MatchConfig {
    fn default() -> Self {
        MatchConfig {
            max_skills: 3,
            max_sops: 3,
            min_score: 0.15,
        }
    }
}

/// Unified matcher that searches across skills and SOPs.
pub struct Matcher;

impl Matcher {
    /// Match skills against a query.
    pub fn match_skills<'a>(skills: &'a [Skill], query: &str) -> Vec<(f32, &'a Skill)> {
        let mut scored: Vec<(f32, &Skill)> = skills
            .iter()
            .filter(|s| s.metadata.is_active())
            .map(|s| (s.match_score(query), s))
            .filter(|(score, _)| *score > 0.0)
            .collect();

        scored.sort_by(|(a, _), (b, _)| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        scored
    }

    /// Match SOPs against a query.
    pub fn match_sops<'a>(sops: &'a [Sop], query: &str) -> Vec<(f32, &'a Sop)> {
        let mut scored: Vec<(f32, &Sop)> = sops
            .iter()
            .filter(|s| s.metadata.is_active())
            .map(|s| (s.match_score(query), s))
            .filter(|(score, _)| *score > 0.0)
            .collect();

        scored.sort_by(|(a, _), (b, _)| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        scored
    }

    /// Build a combined prompt snippet from skills and SOPs.
    pub fn build_combined_snippet(
        skills: &[Skill],
        sops: &[Sop],
        query: &str,
        config: &MatchConfig,
    ) -> String {
        let skill_matches = Self::match_skills(skills, query);
        let sop_matches = Self::match_sops(sops, query);

        let mut snippet = String::new();

        // Inject SOPs FIRST — content before wrapper so actionable command is at top
        let injectable_sops: Vec<_> = sop_matches
            .iter()
            .filter(|(s, _)| *s >= config.min_score)
            .take(config.max_sops)
            .collect();

        for (_score, sop) in &injectable_sops {
            snippet.push_str(&sop.content);
            snippet.push_str("\n\n");
        }

        if !injectable_sops.is_empty() {
            snippet.push_str("<SYSTEM_OVERRIDE>\n");
            snippet.push_str(
                "The content above is your SOP. It IS your instruction. Follow it exactly.\n",
            );
            snippet.push_str(
                "You already have the SOP — do NOT call skill_mcp_search or read files.\n",
            );
            snippet.push_str(
                "If SOP says run a bash command, run it NOW. If SOP says ask user, ask NOW.\n",
            );
            snippet.push_str("</SYSTEM_OVERRIDE>\n\n");
        }

        // Inject skills
        let injectable_skills: Vec<_> = skill_matches
            .iter()
            .filter(|(s, _)| *s >= config.min_score)
            .take(config.max_skills)
            .collect();

        if !injectable_skills.is_empty() {
            snippet.push_str("## Available Skills\n\n");
            for (score, skill) in &injectable_skills {
                snippet.push_str(&format!(
                    "--- Skill: {} (score: {:.2}) ---\n\n",
                    skill.name, score
                ));
                snippet.push_str(&skill.to_prompt_snippet());
                snippet.push('\n');
            }
        }

        snippet
    }

    /// Compute keyword overlap score (Jaccard similarity) between two strings.
    /// Used for simple similarity without full TF-IDF.
    pub fn keyword_overlap(a: &str, b: &str) -> f32 {
        let words_a = tokenize(a);
        let words_b = tokenize(b);

        if words_a.is_empty() || words_b.is_empty() {
            return 0.0;
        }

        let intersection: usize = words_a.keys().filter(|w| words_b.contains_key(*w)).count();
        let union: usize = words_a
            .keys()
            .chain(words_b.keys())
            .collect::<std::collections::HashSet<_>>()
            .len();

        if union == 0 {
            0.0
        } else {
            intersection as f32 / union as f32
        }
    }
}

fn tokenize(s: &str) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    for word in s.to_lowercase().split_whitespace() {
        let clean: String = word.chars().filter(|c| c.is_alphanumeric()).collect();
        if clean.is_empty() {
            continue;
        }
        if clean.chars().any(is_cjk) {
            let mut ascii_buf = String::new();
            for ch in clean.chars() {
                if is_cjk(ch) {
                    if ascii_buf.len() >= 2 {
                        *map.entry(ascii_buf.clone()).or_insert(0) += 1;
                    }
                    ascii_buf.clear();
                    *map.entry(ch.to_string()).or_insert(0) += 1;
                } else {
                    ascii_buf.push(ch);
                }
            }
            if ascii_buf.len() >= 2 {
                *map.entry(ascii_buf).or_insert(0) += 1;
            }
        } else if clean.len() >= 2 {
            *map.entry(clean).or_insert(0) += 1;
        }
    }
    map
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch,
        '\u{4E00}'..='\u{9FFF}'
        | '\u{3400}'..='\u{4DBF}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{3040}'..='\u{309F}'
        | '\u{30A0}'..='\u{30FF}'
        | '\u{AC00}'..='\u{D7AF}'
    )
}

// ── Query tokenisation for relevance scoring ─────────────────────────────
//
// Chinese has no whitespace, so `split_whitespace` turns a whole sentence into
// one token that matches nothing — every Chinese query used to score a flat
// zero. Candidates are tokenised in graded levels and the first level that
// produces any signal wins:
//
//   level 0  CJK bigrams + Latin words    — specific enough to rank
//   level 1  CJK characters + Latin words — recall when bigrams miss
//   level 2  whitespace words             — legacy / Latin-only queries
//
// Bigrams before characters: a single CJK character occurs in almost every
// document, so per-character scoring saturates every candidate at the same
// value and the ranking collapses (measured: R@3 0.54 -> 0.46). Bigrams stay
// discriminative while still cutting a query that has no spaces.

/// Push an accumulated Latin run, keeping only words of length >= 2.
fn take_word(out: &mut Vec<String>, buf: &mut String) {
    let w = buf.trim();
    if w.len() >= 2 {
        out.push(w.to_string());
    }
    buf.clear();
}

/// Sliding `n`-grams over a CJK run; a short run falls back to itself.
fn cjk_ngrams(run: &str, n: usize) -> Vec<String> {
    let cs: Vec<char> = run.chars().collect();
    if cs.len() < n {
        return vec![run.to_string()];
    }
    (0..=cs.len() - n).map(|i| cs[i..i + n].iter().collect()).collect()
}

fn dedup_preserve_order(v: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    v.into_iter().filter(|t| seen.insert(t.clone())).collect()
}

/// Level 0 — CJK bigrams plus Latin words (len >= 2).
pub fn tok_bigram(query: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for chunk in query.to_lowercase().split_whitespace() {
        let clean: String = chunk.chars().filter(|c| c.is_alphanumeric()).collect();
        if clean.is_empty() {
            continue;
        }
        if clean.chars().any(is_cjk) {
            let mut run = String::new();
            let mut latin = String::new();
            for ch in clean.chars() {
                if is_cjk(ch) {
                    take_word(&mut out, &mut latin);
                    run.push(ch);
                } else {
                    if !run.is_empty() {
                        out.extend(cjk_ngrams(&run, 2));
                        run.clear();
                    }
                    latin.push(ch);
                }
            }
            if !run.is_empty() {
                out.extend(cjk_ngrams(&run, 2));
            }
            take_word(&mut out, &mut latin);
        } else if clean.len() >= 2 {
            out.push(clean);
        }
    }
    dedup_preserve_order(out)
}

/// Level 1 — one token per CJK character, plus Latin words (len >= 2).
pub fn tok_char(query: &str) -> Vec<String> {
    // `tokenize` counts occurrences in a HashMap; sort for a deterministic
    // order so scores are reproducible across runs.
    let mut v: Vec<String> = tokenize(query).into_keys().collect();
    v.sort();
    v
}

/// Level 2 — legacy whitespace split (Latin behaviour, byte-for-byte as before).
pub fn tok_word(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split_whitespace()
        .map(|w| w.to_string())
        .collect()
}

/// Graded tokenisation, coarse-to-fine: `[bigrams, characters, words]`.
pub fn tokenize_levels(query: &str) -> [Vec<String>; 3] {
    [tok_bigram(query), tok_char(query), tok_word(query)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keyword_overlap_identical() {
        let score = Matcher::keyword_overlap("hello world", "world hello");
        assert!(score > 0.8);
    }

    #[test]
    fn test_keyword_overlap_no_match() {
        let score = Matcher::keyword_overlap("hello world", "foo bar baz");
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_keyword_overlap_partial() {
        let score = Matcher::keyword_overlap("search the web", "search files on disk");
        assert!(score > 0.0);
        assert!(score < 1.0);
    }

    #[test]
    fn test_tokenize_levels_chinese_splits() {
        let lv = tokenize_levels("帮我下载大模型");
        // level 0 must split the sentence into bigrams, never keep it whole
        assert!(lv[0].len() > 1, "bigram level should split: {:?}", lv[0]);
        assert!(!lv[0].contains(&"帮我下载大模型".to_string()), "{:?}", lv[0]);
        assert!(lv[0].iter().any(|t| t.chars().count() == 2), "{:?}", lv[0]);
    }

    #[test]
    fn test_tokenize_levels_latin_unchanged() {
        // Latin queries must tokenise exactly as the legacy whitespace split
        assert_eq!(
            tokenize_levels("search the internet")[0],
            vec!["search", "the", "internet"]
        );
    }

    #[test]
    fn test_tokenize_levels_mixed() {
        let lv = tokenize_levels("用 aria2c 下载模型");
        assert!(lv[0].iter().any(|t| t == "aria2c"), "{:?}", lv[0]);
        assert!(lv[0].iter().any(|t| t == "下载"), "{:?}", lv[0]);
    }

    #[test]
    fn test_tokenize_levels_three_levels_always_present() {
        let lv = tokenize_levels("任意查询");
        assert_eq!(lv.len(), 3);
        assert!(!lv[1].is_empty(), "char level must be non-empty: {:?}", lv[1]);
    }

    #[test]
    fn test_match_config_default() {
        let cfg = MatchConfig::default();
        assert_eq!(cfg.max_skills, 3);
        assert_eq!(cfg.max_sops, 3);
        assert!(cfg.min_score > 0.0);
    }
}
