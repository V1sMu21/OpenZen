//! Seeded property tests: generate from the *space*, not from examples.
//!
//! No `proptest` dependency: a 64-bit LCG is enough, and being explicit about
//! the seed keeps failures replayable with `OZ_ECF_SEED`.

use serde_json::{json, Map, Value};

use oz_testkit::aliases;
use oz_testkit::gen_bytes::{self, Op};
use oz_testkit::props;

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

fn seed() -> u64 {
    std::env::var("OZ_ECF_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0xECF_2026_1117)
}

#[test]
fn canonicalization_is_idempotent_and_never_invents() {
    let mut rng = Lcg(seed());
    let tools = &aliases::TOOL_ALIASES;
    for _ in 0..2_000 {
        let t = &tools[rng.pick(tools.len())];
        let groups: Vec<&_> = t.groups.iter().filter(|g| !g.aliases.is_empty()).collect();
        if groups.is_empty() {
            continue;
        }
        let mut obj = Map::new();
        let g = groups[rng.pick(groups.len())];
        // Pick either the canonical key or one of its aliases, sometimes both.
        let key = if rng.pick(2) == 0 {
            g.canonical
        } else {
            g.aliases[rng.pick(g.aliases.len())]
        };
        let canonicals = aliases::canonical_names(t.tool);
        let used_alias = key != g.canonical && !canonicals.contains(&key);
        let value = match rng.pick(4) {
            0 => json!(format!("v{}", rng.pick(1000))),
            1 => json!(rng.pick(1000)),
            2 => Value::Null,
            _ => json!([1, 2]),
        };
        obj.insert(key.to_string(), value.clone());
        if rng.pick(3) == 0 {
            obj.insert(g.canonical.to_string(), json!("canonical"));
        }
        if rng.pick(5) == 0 {
            obj.insert("unknown_extra".into(), json!("kept"));
        }
        let canonical_absent = !obj.contains_key(g.canonical);
        let input = Value::Object(obj);
        let once = oz_core_types::tool_args::canonicalize_tool_args(t.tool, input.clone());
        if used_alias && canonical_absent && !g.canonical.is_empty() {
            assert_eq!(
                once.get(g.canonical),
                Some(&value),
                "{}: alias `{key}` did not move to `{}` (identity stub?)",
                t.tool,
                g.canonical
            );
        }
        let twice = oz_core_types::tool_args::canonicalize_tool_args(t.tool, once.clone());
        assert_eq!(once, twice, "{} not idempotent for {input}", t.tool);
        let errs = props::check_canonicalize(&input, &once, &twice);
        assert!(errs.is_empty(), "{}: {errs:?}", t.tool);
        if input.get("unknown_extra").is_some() {
            // An unknown key is either preserved as-is, or (when it is the
            // lone unrecognized string key) adopted as the primary parameter.
            // Either way its value must survive — dropping it is the bug.
            assert!(
                once.to_string().contains("kept"),
                "{} lost the unknown key's value: {once}",
                t.tool
            );
        }
    }
}

#[test]
fn repair_survives_random_mutations_without_inventing_content() {
    let fixtures = gen_bytes::load_fixtures().expect("fixtures missing");
    let names: Vec<&String> = fixtures.keys().collect();
    let mut rng = Lcg(seed() ^ 0x9E37_79B9);
    let ops = 500usize;
    for i in 0..ops {
        let name = names[rng.pick(names.len())];
        let text = &fixtures[name];
        let mut recipe = Vec::new();
        for _ in 0..(1 + rng.pick(3)) {
            match rng.pick(10) {
                0 => recipe.push(Op::Truncate {
                    at: 1 + rng.pick(text.len().max(2)),
                }),
                1 => recipe.push(Op::Closer {
                    to: ["</", "", "</｜DSML", "</parameter>"][rng.pick(4)].to_string(),
                }),
                2 => recipe.push(Op::ReplaceBar),
                3 => recipe.push(Op::PrefixJunk {
                    text: "{\"response\">".into(),
                }),
                4 => recipe.push(Op::SuffixJunk {
                    text: "\n</invoke>".into(),
                }),
                5 => recipe.push(Op::Nul {
                    at: 1 + rng.pick(text.len().max(2)),
                }),
                6 => recipe.push(Op::WrapJson),
                7 => recipe.push(Op::HtmlEscape),
                8 => recipe.push(Op::DropFirst),
                _ => recipe.push(Op::JsonEscape),
            }
        }
        let mutated = gen_bytes::apply(text, &recipe);
        if mutated.len() > 64 * 1024 {
            continue;
        }
        let tool = if name.starts_with("ask_user") {
            Some("ask_user")
        } else if name.starts_with("write") {
            Some("write")
        } else if name.starts_with("panel") {
            Some("open_side_panel")
        } else {
            None
        };
        let got = oz_llm::stream::repair_parameter_style_args(&mutated, tool);
        let errs = props::check_repair(&mutated, got.as_ref());
        assert!(
            errs.is_empty(),
            "iteration {i} ({name}, {:?}) violated: {errs:?}",
            recipe
        );
    }
}

#[test]
fn missing_arg_error_names_the_key_and_what_was_sent() {
    let args = json!({"file_path": "a.txt"});
    let msg = oz_core_types::tool_args::missing_arg_error("write", &["content"], &args);
    for needle in ["content", "file_path", "re-issue"] {
        assert!(msg.contains(needle), "error must mention {needle}: {msg}");
    }
}
