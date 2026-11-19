//! `oz-casegen` — mine the contract, generate cases/corpus, validate the库.

use std::collections::BTreeMap;
use std::path::PathBuf;

use oz_testkit::{case, contract::Contract, gen_bytes, gen_params, gen_security, repo_root, spec};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("check");
    let code = match cmd {
        "contract" => contract_cmd(&args),
        "params" => params_cmd(&args),
        "bytes" => bytes_cmd(&args),
        "spec" => spec_cmd(&args),
        "all" => all_cmd(),
        "check" => check_cmd(),
        "-h" | "--help" | "help" => {
            println!(
                "oz-casegen <contract|params|bytes|spec|all|check>\n\
                 \n\
                 all     regenerate contract, tool-arg cases, wire corpus, spec drafts, manifest\n\
                 check   validate the corpus and detect contract drift (exit 4 on failure)"
            );
            0
        }
        other => {
            eprintln!("unknown command: {other}");
            2
        }
    };
    std::process::exit(code);
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn contract_cmd(args: &[String]) -> i32 {
    let out = flag(args, "--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("contract/contract.json"));
    let c = Contract::mine();
    let errs = c.check();
    if !errs.is_empty() {
        for e in &errs {
            eprintln!("CONTRACT FAIL: {e}");
        }
        return 4;
    }
    if let Err(e) = c.save(&out) {
        eprintln!("{e}");
        return 4;
    }
    println!(
        "contract: {} tools, fingerprint {}, wrote {}",
        c.tools.len(),
        c.fingerprint(),
        out.display()
    );
    0
}

fn params_cmd(args: &[String]) -> i32 {
    let out = flag(args, "--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("cases/unit/tool-args.jsonl"));
    let mut cases = gen_params::generate();
    cases.extend(gen_security::generate());
    if let Err(e) = case::write_jsonl(&out, &cases) {
        eprintln!("{e}");
        return 4;
    }
    println!("params: {} cases → {}", cases.len(), out.display());
    0
}

fn bytes_cmd(args: &[String]) -> i32 {
    let out = flag(args, "--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("corpus/wire/seeds.jsonl"));
    let limit: usize = flag(args, "--limit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(5000);
    let seeds = match gen_bytes::generate(limit) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return 4;
        }
    };
    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut body = String::new();
    for s in &seeds {
        match serde_json::to_string(s) {
            Ok(j) => {
                body.push_str(&j);
                body.push('\n');
            }
            Err(e) => {
                eprintln!("{e}");
                return 4;
            }
        }
    }
    if let Err(e) = std::fs::write(&out, body) {
        eprintln!("{}: {e}", out.display());
        return 4;
    }
    let fixtures = gen_bytes::load_fixtures().map(|f| f.len()).unwrap_or(0);
    println!(
        "wire: {} seeds from {} fixtures → {}",
        seeds.len(),
        fixtures,
        out.display()
    );
    0
}

fn spec_cmd(args: &[String]) -> i32 {
    let md = flag(args, "--md")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("docs/acceptance-criteria.md"));
    let dir = flag(args, "--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("cases/spec"));
    let cases = match spec::mine(&md) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 4;
        }
    };
    if let Err(e) = spec::write_drafts(&cases, &dir) {
        eprintln!("{e}");
        return 4;
    }
    println!("spec: {} draft scenarios → {}", cases.len(), dir.display());
    0
}

fn all_cmd() -> i32 {
    let mut code = contract_cmd(&[]);
    if code != 0 {
        return code;
    }
    code = params_cmd(&[]);
    if code != 0 {
        return code;
    }
    code = bytes_cmd(&[]);
    if code != 0 {
        return code;
    }
    code = spec_cmd(&[]);
    if code != 0 {
        return code;
    }
    match write_manifest() {
        Ok(n) => {
            println!("manifest: {n} files indexed");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            4
        }
    }
}

fn write_manifest() -> Result<usize, String> {
    let contract = Contract::load(&repo_root().join("contract/contract.json"))?;
    let unit = oz_testkit::load_unit_cases()?;
    let wire = oz_testkit::load_wire()?;
    let drafts = oz_testkit::load_spec_cases()?;
    let mut by_layer: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_fc: BTreeMap<String, usize> = BTreeMap::new();
    for c in &unit {
        by_layer
            .entry(format!("{:?}", c.layer).to_lowercase())
            .and_modify(|v| *v += 1)
            .or_insert(1);
        for fc in &c.fc {
            by_fc.entry(fc.clone()).and_modify(|v| *v += 1).or_insert(1);
        }
    }
    let manifest = serde_json::json!({
        "version": 1,
        "contract_fingerprint": contract.fingerprint(),
        "tools": contract.tools.len(),
        "unit_cases": unit.len(),
        "wire_seeds": wire.len(),
        "spec_drafts": drafts.len(),
        "by_layer": by_layer,
        "by_fc": by_fc,
    });
    let path = repo_root().join("cases/manifest.json");
    let mut s = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    s.push('\n');
    std::fs::write(&path, s).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(unit.len() + wire.len() + drafts.len() + 1)
}

fn check_cmd() -> i32 {
    let root = repo_root();
    let mut errors: Vec<String> = Vec::new();

    let cpath = root.join("contract/contract.json");
    let contract = match Contract::load(&cpath) {
        Ok(c) => Some(c),
        Err(e) => {
            errors.push(e);
            None
        }
    };
    if let Some(c) = &contract {
        errors.extend(c.check());
        let live = Contract::mine();
        if live.fingerprint() != c.fingerprint() {
            errors.push(format!(
                "contract drift: checked-in {} != live {} — run `oz-casegen contract`",
                c.fingerprint(),
                live.fingerprint()
            ));
        }
    }

    let unit = match oz_testkit::load_unit_cases() {
        Ok(c) => c,
        Err(e) => {
            errors.push(e);
            Vec::new()
        }
    };
    for c in &unit {
        errors.extend(c.validate());
    }
    for id in case::duplicate_ids(&unit) {
        errors.push(format!("duplicate case id: {id}"));
    }

    let wire = match oz_testkit::load_wire() {
        Ok(w) => w,
        Err(e) => {
            errors.push(e);
            Vec::new()
        }
    };
    let fixtures = gen_bytes::load_fixtures().unwrap_or_default();
    let mut wire_ids = std::collections::BTreeSet::new();
    for s in &wire {
        if !wire_ids.insert(s.id.clone()) {
            errors.push(format!("duplicate wire seed id: {}", s.id));
        }
        if !fixtures.contains_key(&s.source) {
            errors.push(format!("{}: missing fixture {}", s.id, s.source));
        }
        if s.ops.len() > 6 {
            errors.push(format!("{}: {} ops (max 6)", s.id, s.ops.len()));
        }
    }

    let drafts = oz_testkit::load_spec_cases().unwrap_or_default();
    for c in &drafts {
        errors.extend(c.validate());
    }

    match std::fs::read_to_string(root.join("cases/manifest.json")) {
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(m) => {
                let checks = [
                    ("unit_cases", unit.len()),
                    ("wire_seeds", wire.len()),
                    ("spec_drafts", drafts.len()),
                ];
                for (key, actual) in checks {
                    let want = m.get(key).and_then(|v| v.as_u64()).unwrap_or(u64::MAX) as usize;
                    if want != actual {
                        errors.push(format!(
                            "manifest.{key}={want} but corpus has {actual} — run `oz-casegen all`"
                        ));
                    }
                }
            }
            Err(e) => errors.push(format!("cases/manifest.json: {e}")),
        },
        Err(e) => errors.push(format!("cases/manifest.json: {e} — run `oz-casegen all`")),
    }

    if errors.is_empty() {
        println!(
            "check: OK — {} unit cases, {} wire seeds, {} spec drafts, contract {}",
            unit.len(),
            wire.len(),
            drafts.len(),
            contract.map(|c| c.fingerprint()).unwrap_or_default()
        );
        0
    } else {
        for e in &errors {
            eprintln!("FAIL: {e}");
        }
        eprintln!("check: {} problem(s)", errors.len());
        4
    }
}
