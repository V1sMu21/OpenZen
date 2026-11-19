//! Executes one case and turns it into an `Outcome`.
//!
//! Only pure, in-process targets live here (canonicalize / repair /
//! normalize / missing-arg). E2E and UI cases are executed by the Python
//! matrix and vitest respectively; their results are merged into the same
//! report, so the IR stays the single vocabulary for all layers.

use std::time::Instant;

use serde_json::Value;

use crate::case::{Case, CaseStatus, Expect, Input, Layer, TargetKind};
use crate::gen_bytes::{self, WireSeed};
use crate::props;
use crate::report::Outcome;

pub fn run_case(c: &Case) -> Outcome {
    let layer = layer_tag(c.layer);
    let sev = format!("{:?}", c.severity);
    if c.status != CaseStatus::Calibrated {
        return Outcome::pass(&c.id, layer, &c.fc, &sev, 0)
            .skip("case is draft/retired; not executed")
            .with_repro(&c.repro);
    }
    let started = Instant::now();
    let base = || {
        Outcome::pass(
            &c.id,
            layer,
            &c.fc,
            &sev,
            started.elapsed().as_millis() as u64,
        )
        .with_repro(&c.repro)
    };
    let budget = c.budget_ms.unwrap_or(500);

    match c.target.kind {
        TargetKind::Canonicalize => {
            let Input::Json { value } = &c.input else {
                return base().error("schema", "canonicalize needs json input");
            };
            let tool = c.target.tool.as_deref().unwrap_or("");
            let once = oz_core_types::tool_args::canonicalize_tool_args(tool, value.clone());
            let twice = oz_core_types::tool_args::canonicalize_tool_args(tool, once.clone());
            if let Some(msg) = props::check_canonicalize(value, &once, &twice)
                .into_iter()
                .next()
            {
                return base().fail(value.clone(), once.clone(), &msg);
            }
            match &c.expect {
                Expect::Exact {
                    value: want,
                    forbid_keys,
                } => {
                    for k in forbid_keys {
                        if once.get(k).is_some() {
                            return base().fail(
                                want.clone(),
                                once.clone(),
                                &format!("forbidden key `{k}` survived canonicalization"),
                            );
                        }
                    }
                    if &once != want {
                        return base().fail(
                            want.clone(),
                            once.clone(),
                            "output != contract oracle",
                        );
                    }
                }
                Expect::Contains { text } => {
                    let s = once.to_string();
                    for t in text {
                        if !s.contains(t) {
                            return base().fail(
                                Value::String(t.clone()),
                                once.clone(),
                                "expected fragment missing",
                            );
                        }
                    }
                }
                other => {
                    return base()
                        .error("schema", &format!("canonicalize cannot satisfy {other:?}"))
                }
            }
            base()
        }
        TargetKind::Repair => {
            let Input::Text { text } = &c.input else {
                return base().error("schema", "repair needs text input");
            };
            let got = oz_llm::stream::repair_parameter_style_args(text, c.target.tool.as_deref());
            let ms = started.elapsed().as_millis() as u64;
            if ms > budget {
                return base().error("timeout", &format!("repair took {ms} ms (budget {budget})"));
            }
            match &c.expect {
                Expect::Properties => {
                    let errs = props::check_repair(text, got.as_ref());
                    if let Some(msg) = errs.into_iter().next() {
                        return base().fail(
                            Value::String("six properties hold".into()),
                            got.unwrap_or(Value::Null),
                            &msg,
                        );
                    }
                }
                Expect::Some => {
                    if got.is_none() {
                        return base().fail(
                            Value::String("Some".into()),
                            Value::Null,
                            "repair returned None",
                        );
                    }
                }
                Expect::None => {
                    if got.is_some() {
                        return base().fail(
                            Value::Null,
                            got.unwrap_or(Value::Null),
                            "repair should have refused",
                        );
                    }
                }
                Expect::Exact { value: want, .. } => {
                    let Some(got) = got else {
                        return base().fail(want.clone(), Value::Null, "repair returned None");
                    };
                    if &got != want {
                        return base().fail(want.clone(), got, "recovered object != expected");
                    }
                }
                other => {
                    return base().error("schema", &format!("repair cannot satisfy {other:?}"))
                }
            }
            base()
        }
        TargetKind::Normalize => {
            let Input::Json { value } = &c.input else {
                return base().error("schema", "normalize needs json input");
            };
            let tool = c.target.tool.as_deref().unwrap_or("");
            let got = oz_llm::stream::normalize_tool_args_for(tool, value.clone());
            match &c.expect {
                Expect::Exact { value: want, .. } => {
                    if &got != want {
                        return base().fail(want.clone(), got.clone(), "normalized != expected");
                    }
                }
                Expect::Properties => {}
                other => {
                    return base().error("schema", &format!("normalize cannot satisfy {other:?}"))
                }
            }
            base()
        }
        TargetKind::MissingArg => {
            let Input::Args { value } = &c.input else {
                return base().error("schema", "missing_arg needs args input");
            };
            let tool = c.target.tool.as_deref().unwrap_or("");
            let (missing, sent) = match &c.expect {
                Expect::MissingArg { missing, sent } => (missing.clone(), sent.clone()),
                other => {
                    return base().error("schema", &format!("missing_arg cannot satisfy {other:?}"))
                }
            };
            let missing_refs: Vec<&str> = missing.iter().map(String::as_str).collect();
            let msg = oz_core_types::tool_args::missing_arg_error(tool, &missing_refs, value);
            for part in &missing {
                if !msg.contains(part) {
                    return base().fail(
                        Value::String(part.clone()),
                        Value::String(msg),
                        "missing key not named",
                    );
                }
            }
            for key in &sent {
                if !msg.contains(key) {
                    return base().fail(
                        Value::String(key.clone()),
                        Value::String(msg.clone()),
                        "sent key not listed",
                    );
                }
            }
            base()
        }
        TargetKind::Pattern => {
            let Input::Json { value } = &c.input else {
                return base().error("schema", "pattern needs json input");
            };
            let tool = c.target.tool.as_deref().unwrap_or("");
            let (t, p) = oz_safety::patterns::build_pattern(tool, value);
            match &c.expect {
                Expect::Exact { value: want, .. } => {
                    let got = serde_json::json!([t, p]);
                    if &got != want {
                        return base().fail(want.clone(), got, "pattern != expected");
                    }
                }
                Expect::Contains { text } => {
                    let joined = format!("{t}/{p}");
                    for needle in text {
                        if !joined.contains(needle) {
                            return base().fail(
                                Value::String(needle.clone()),
                                Value::String(joined.clone()),
                                "pattern fragment missing",
                            );
                        }
                    }
                }
                other => {
                    return base().error("schema", &format!("pattern cannot satisfy {other:?}"))
                }
            }
            base()
        }
        TargetKind::Outside => {
            let Input::Json { value } = &c.input else {
                return base().error("schema", "outside needs json input");
            };
            let tool = c.target.tool.as_deref().unwrap_or("");
            let wd = c.target.working_dir.as_deref().unwrap_or("/tmp/ws");
            let got = oz_core_types::path_trust::outside_workspace_target(tool, value, wd);
            match &c.expect {
                Expect::Some => {
                    if got.is_none() {
                        return base().fail(
                            Value::String("Some".into()),
                            Value::Null,
                            "path was treated as inside",
                        );
                    }
                }
                Expect::None => {
                    if let Some(p) = got {
                        return base().fail(
                            Value::String("None".into()),
                            Value::String(p.display().to_string()),
                            "an inside path was treated as outside",
                        );
                    }
                }
                Expect::Exact { value: want, .. } => {
                    let actual = got
                        .map(|p| Value::String(p.display().to_string()))
                        .unwrap_or(Value::Null);
                    if &actual != want {
                        return base().fail(want.clone(), actual, "resolved path != expected");
                    }
                }
                other => {
                    return base().error("schema", &format!("outside cannot satisfy {other:?}"))
                }
            }
            base()
        }
        TargetKind::E2e | TargetKind::Ui => {
            base().skip("executed by the E2E/UI harness, not this runner")
        }
    }
}

/// Run one wire seed against `repair`. The six properties are the oracle.
pub fn run_wire(seed: &WireSeed, fixtures: &std::collections::BTreeMap<String, String>) -> Outcome {
    let mut fc = vec!["FC-B2".to_string()];
    if seed
        .ops
        .iter()
        .any(|o| matches!(o, crate::gen_bytes::Op::Repeat { .. }))
    {
        fc.push("FC-E2".to_string());
    }
    let Some(src) = fixtures.get(&seed.source) else {
        return Outcome::pass(&seed.id, "prop", &fc, "P2", 0)
            .error("infra", &format!("fixture {} missing", seed.source));
    };
    let text = gen_bytes::apply(src, &seed.ops);
    if text.len() > 64 * 1024 {
        return Outcome::pass(&seed.id, "prop", &fc, "P2", 0).skip("mutated seed exceeds 64 KiB");
    }
    let started = Instant::now();
    let got = oz_llm::stream::repair_parameter_style_args(&text, seed.tool.as_deref());
    let ms = started.elapsed().as_millis() as u64;
    let o = Outcome::pass(&seed.id, "prop", &fc, "P2", ms).with_repro(&format!(
        "cargo run -p oz-testkit --bin oz-caselab -- run --case {}",
        seed.id
    ));
    if ms > 200 {
        return o.error(
            "timeout",
            &format!("repair took {ms} ms for {} bytes", text.len()),
        );
    }
    if seed.must_recover && got.is_none() {
        return o.fail(
            Value::String("Some".into()),
            Value::Null,
            "unmutated fixture must be recoverable; repair refused it",
        );
    }
    let errs = props::check_repair(&text, got.as_ref());
    if let Some(msg) = errs.into_iter().next() {
        return o.fail(
            Value::String("six properties hold".into()),
            got.unwrap_or(Value::Null),
            &msg,
        );
    }
    o
}

/// Physical layer tag for cases that are not unit tests.
pub fn layer_tag(l: Layer) -> &'static str {
    match l {
        Layer::Unit => "unit",
        Layer::Prop => "prop",
        Layer::Model => "model",
        Layer::E2e => "e2e",
        Layer::Ui => "ui",
        Layer::Spec => "spec",
    }
}
