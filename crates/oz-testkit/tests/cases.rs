//! The corpus is the test. A missing corpus is a failure, never a skip.

use oz_testkit::{
    case, contract::Contract, gen_bytes, load_unit_cases, load_wire, repo_root, runner,
};

#[test]
fn every_unit_case_passes() {
    let cases = load_unit_cases().expect("cases/unit is missing — run `oz-casegen all`");
    assert!(!cases.is_empty(), "corpus is empty — run `oz-casegen all`");
    let mut errs = Vec::new();
    for c in &cases {
        errs.extend(c.validate());
    }
    for id in case::duplicate_ids(&cases) {
        errs.push(format!("duplicate id {id}"));
    }
    assert!(errs.is_empty(), "invalid cases:\n{}", errs.join("\n"));

    let mut failures = Vec::new();
    for c in &cases {
        let o = runner::run_case(c);
        if o.status != oz_testkit::report::Status::Pass {
            failures.push(format!(
                "{}: {:?} — {}",
                o.case_id,
                o.status,
                o.error
                    .as_ref()
                    .map(|e| e.message.as_str())
                    .unwrap_or_default()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} case(s) failed:\n{}",
        failures.len(),
        failures
            .iter()
            .take(12)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn wire_corpus_properties_hold() {
    let seeds = load_wire().expect("corpus/wire is missing — run `oz-casegen all`");
    assert!(
        !seeds.is_empty(),
        "wire corpus is empty — run `oz-casegen all`"
    );
    let fixtures = gen_bytes::load_fixtures().expect("fixtures missing");
    // PR smoke budget: 1,000 seeds. The release gate runs the whole corpus
    // through `oz-caselab --tier nightly`, which is where the 5k live.
    let limit = std::env::var("OZ_ECF_FULL_WIRE")
        .ok()
        .map(|v| v == "1")
        .map(|full| if full { usize::MAX } else { 1000 })
        .unwrap_or(1000);
    let mut failures = Vec::new();
    for s in seeds.iter().take(limit) {
        let o = runner::run_wire(s, &fixtures);
        if o.status == oz_testkit::report::Status::Fail {
            failures.push(format!(
                "{} ({}): {}",
                o.case_id,
                s.source,
                o.error
                    .as_ref()
                    .map(|e| e.message.as_str())
                    .unwrap_or_default()
            ));
        }
        if o.status == oz_testkit::report::Status::Error {
            panic!("infrastructure error on {}: {:?}", o.case_id, o.error);
        }
    }
    assert!(
        failures.is_empty(),
        "{} wire seed(s) violated the properties:\n{}",
        failures.len(),
        failures
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn checked_in_contract_matches_the_live_registry() {
    let path = repo_root().join("contract/contract.json");
    let saved = Contract::load(&path)
        .expect("contract/contract.json is missing — run `oz-casegen contract`");
    let live = Contract::mine();
    assert_eq!(
        saved.fingerprint(),
        live.fingerprint(),
        "tool contract drifted; run `oz-casegen contract` and review the diff"
    );
    assert!(saved.check().is_empty(), "{:?}", saved.check());
}
