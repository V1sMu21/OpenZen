//! Interactive-ish demo of the steward's public API.
//! Run: cargo run -p oz-steward --example steward_demo

use std::sync::Arc;

use oz_core_types::supervision::{now_ms, Phase, StepRecord, StepStatus, ToolRecord};
use oz_steward::contract::{Acceptance, RiskLevel, TaskContract};
use oz_steward::intervene::CorrectionEngine;
use oz_steward::judge::Action;
use oz_steward::report::{ReportComposer, ReportStatus};
use oz_steward::{Store, Supervisor};

fn step(id: &str, tool: &str, target: &str, at: i64) -> StepRecord {
    StepRecord {
        id: id.into(),
        ticket_id: "demo".into(),
        session_id: "demo-session".into(),
        turn: 1,
        phase: Phase::Acting,
        at,
        tool: Some(ToolRecord {
            name: tool.into(),
            args_hash: format!("{tool}-{target}"),
            output_hash: None,
            duration_ms: Some(10),
            status: StepStatus::Succeeded,
        }),
        status: StepStatus::Succeeded,
        artifacts: vec![target.into()],
        targets: vec![target.into()],
        plan_hash: None,
        git_sha: None,
        tokens_in: 100,
        tokens_out: 100,
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let store = Arc::new(Store::open_memory()?);
    store.ensure_ticket("demo", "demo-session", "修复登录页")?;

    let mut contract = TaskContract::new(
        "demo",
        "修复登录页，只允许改 src/",
        vec![Acceptance {
            id: "a0".into(),
            description: "cargo test 通过".into(),
        }],
        RiskLevel::Low,
        true,
    )?;
    contract.allowed_paths = vec!["src".into()];
    store.save_contract(&contract)?;

    println!(
        "1) 建立合同：goal={} allowed_paths={:?}",
        contract.goal, contract.allowed_paths
    );
    let now = now_ms();
    store.record_step(&step("s1", "read", "src/login.ts", now))?;
    store.record_step(&step("s2", "read", "src/login.ts", now + 1))?;
    store.record_step(&step("s3", "read", "src/login.ts", now + 2))?;
    store.record_step(&step("s4", "write", "Cargo.toml", now + 3))?;
    println!("2) 已记 4 步：3 次重复 read + 1 次越界 write(Cargo.toml)");

    let sup = Supervisor::new(store.clone());
    let Some(j) = sup.evaluate("demo", false, false)? else {
        println!("no judgment (terminal/no contract)");
        return Ok(());
    };
    println!(
        "3) 确定性 verdict = {:?}, confidence = {:.2}",
        j.verdict, j.confidence
    );
    for f in &j.findings {
        println!(
            "   - {:?}/{:?}: {} (evidence={:?})",
            f.kind, f.severity, f.detail, f.evidence_ids
        );
    }
    match &j.action {
        Some(Action::Nudge(text)) => println!("4) 自动 nudge: {text}"),
        Some(Action::Escalate) => println!("4) 升级 L2：需要重锚，不自动写文件"),
        None => println!("4) 无自动动作"),
    }

    let mut engine = CorrectionEngine::default();
    if let Some(plan) = engine.propose("demo", j.verdict, &j.findings, now + 4) {
        engine.record(&store, &plan)?;
        println!(
            "5) 纠正计划：{} {:?} — {}",
            plan.level.as_str(),
            plan.action,
            plan.reason
        );
    }

    // 第二个例子：只有重复 read，没有越界 -> AtRisk + L1 nudge。
    store.ensure_ticket("demo2", "demo-session-2", "只读检查")?;
    let mut c2 = TaskContract::new(
        "demo2",
        "只读检查 src/",
        vec![Acceptance {
            id: "a1".into(),
            description: "read src/a.ts".into(),
        }],
        RiskLevel::Low,
        true,
    )?;
    c2.allowed_paths = vec!["src".into()];
    store.save_contract(&c2)?;
    for (i, id) in ["r1", "r2", "r3"].iter().enumerate() {
        let mut s = step(id, "read", "src/a.ts", now + 10 + i as i64);
        s.ticket_id = "demo2".into();
        s.session_id = "demo-session-2".into();
        store.record_step(&s)?;
    }
    if let Some(j2) = sup.evaluate("demo2", false, false)? {
        println!("6) 第二个 ticket verdict={:?}", j2.verdict);
        if let Some(Action::Nudge(text)) = &j2.action {
            println!("   L1 nudge: {text}");
        }
    }

    let report = ReportComposer::new(store.clone()).compose("demo", false, vec![])?;
    println!(
        "7) 报告：status={:?}, evidence={} ids",
        report.status,
        report.evidence_ids.len()
    );
    if report.status == ReportStatus::Unverified {
        println!("   没有 gate 证据，所以是 unverified——管家不会替你宣布完成。");
    }
    println!("\n--- IM 摘要 ---\n{}", report.im_summary());
    println!("\n--- Markdown ---\n{}", report.to_markdown());
    Ok(())
}
