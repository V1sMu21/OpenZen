//! `oz-caselab` — execute the corpus and write the report.
//!
//! Exit codes (part of the contract):
//!   0 all pass · 1 product failure · 2 release-gate failure (new P0/P1 or skip)
//!   3 infrastructure error · 4 corpus invalid

use std::collections::BTreeSet;

use oz_testkit::report::{self, Outcome, Run, Status};
use oz_testkit::{case, gen_bytes, load_unit_cases, load_wire, repo_root, runner};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("run") {
        eprintln!("usage: oz-caselab run [--tier pr|nightly|release] [--case ID] [--fc FC] [--limit N] [--no-wire]");
        std::process::exit(2);
    }
    std::process::exit(run_cmd(&args));
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn run_cmd(args: &[String]) -> i32 {
    let tier = flag(args, "--tier").unwrap_or_else(|| "pr".into());
    if !["pr", "nightly", "release"].contains(&tier.as_str()) {
        eprintln!("unknown tier: {tier}");
        return 4;
    }
    let only = flag(args, "--case");
    let fc_filter = flag(args, "--fc");
    let explicit_limit: Option<usize> = flag(args, "--limit").and_then(|s| s.parse().ok());

    let cases = match load_unit_cases() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("corpus: {e}");
            return 4;
        }
    };
    let mut invalid = Vec::new();
    for c in &cases {
        invalid.extend(c.validate());
    }
    for id in case::duplicate_ids(&cases) {
        invalid.push(format!("duplicate case id: {id}"));
    }
    if !invalid.is_empty() {
        for e in invalid {
            eprintln!("corpus FAIL: {e}");
        }
        return 4;
    }

    let selected: Vec<&case::Case> = cases
        .iter()
        .filter(|c| only.as_ref().map(|id| &c.id == id).unwrap_or(true))
        .filter(|c| {
            fc_filter
                .as_ref()
                .map(|fc| c.fc.iter().any(|x| x == fc))
                .unwrap_or(true)
        })
        .collect();
    if let Some(id) = &only {
        if selected.is_empty() && !id.starts_with("WB-") {
            eprintln!("case {id} not found");
            return 4;
        }
    }

    let mut run = Run::new(&tier);
    for c in &selected {
        run.outcomes.push(runner::run_case(c));
    }

    let run_wire = !args.iter().any(|a| a == "--no-wire");
    if run_wire {
        let limit = explicit_limit.unwrap_or(if tier == "pr" { 1000 } else { usize::MAX });
        match (load_wire(), gen_bytes::load_fixtures()) {
            (Ok(seeds), Ok(fixtures)) => {
                let mut n = 0;
                let filtered: Vec<_> = seeds
                    .iter()
                    .filter(|s| only.as_ref().map(|id| &s.id == id).unwrap_or(true))
                    .take(if only.is_some() { usize::MAX } else { limit })
                    .collect::<Vec<_>>();
                for s in filtered {
                    if let Some(id) = &only {
                        if &s.id != id {
                            continue;
                        }
                    }
                    if let Some(fc) = &fc_filter {
                        if fc != "FC-B2" && fc != "FC-E2" {
                            continue;
                        }
                    }
                    run.outcomes.push(runner::run_wire(s, &fixtures));
                    n += 1;
                }
                eprintln!("wire: ran {n} seed(s)");
            }
            (Err(e), _) | (_, Err(e)) => {
                run.outcomes
                    .push(Outcome::pass("WB-000", "prop", &[], "P2", 0).error("infra", &e));
            }
        }
    }

    if tier == "release" {
        let path = repo_root().join("target/caselab/e2e-results.jsonl");
        let mut merged = 0usize;
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                for (i, line) in text.lines().enumerate() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<Outcome>(line) {
                        Ok(o) => {
                            run.outcomes.push(o);
                            merged += 1;
                        }
                        Err(e) => run.outcomes.push(
                            Outcome::pass("E2E-000", "e2e", &[], "P1", 0)
                                .error("infra", &format!("{}:{}: {e}", path.display(), i + 1)),
                        ),
                    }
                }
                eprintln!("e2e: merged {merged} outcome(s)");
                if merged == 0 {
                    run.outcomes
                        .push(Outcome::pass("E2E-000", "e2e", &[], "P1", 0).error(
                            "infra",
                            "e2e-results.jsonl was empty — run scripts/e2e/matrix.sh",
                        ));
                }
            }
            Err(e) => run
                .outcomes
                .push(Outcome::pass("E2E-000", "e2e", &[], "P1", 0).error(
                    "infra",
                    &format!("{}: {e} — run scripts/e2e/matrix.sh", path.display()),
                )),
        }
    }

    if run.outcomes.is_empty() {
        eprintln!("no cases selected — check --case/--fc or the tier filter");
        return 4;
    }

    let baseline_path = flag(args, "--baseline")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repo_root().join("target/caselab/baseline.json"));
    let baseline: BTreeSet<String> = report::load_baseline(&baseline_path);
    run.apply_baseline(&baseline);

    let out_dir = repo_root().join("target/caselab");
    let (jsonl, html) = match run.write(&out_dir) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return 3;
        }
    };
    let (p, f, e, s) = run.counts();
    println!(
        "tier={tier} total={} pass={p} fail={f} error={e} skip={s}",
        run.outcomes.len()
    );
    if f > 0 {
        let new = run
            .outcomes
            .iter()
            .filter(|o| o.status == Status::Fail && !o.known)
            .count();
        println!("failures: {} new vs baseline", new);
        for o in run
            .outcomes
            .iter()
            .filter(|o| o.status == Status::Fail)
            .take(20)
        {
            println!(
                "  {} [{}] {}",
                o.case_id,
                o.severity,
                o.error.as_ref().map(|x| x.message.as_str()).unwrap_or("")
            );
        }
    }
    println!("jsonl: {}", jsonl.display());
    println!("html:  {}", html.display());

    if tier == "release" {
        let new_p0p1 = run.outcomes.iter().any(|o| {
            o.status == Status::Fail && !o.known && (o.severity == "P0" || o.severity == "P1")
        });
        if new_p0p1 || s > 0 {
            eprintln!("release gate: new P0/P1={new_p0p1}, skipped={s} — release requires neither");
            return 2;
        }
    }
    run.exit_code()
}
