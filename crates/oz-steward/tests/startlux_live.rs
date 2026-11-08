//! Live StartLux check. Run only when the official server is up:
//!   STARTLUX_LIVE=1 cargo test -p oz-steward --test startlux_live -- --nocapture

use oz_steward::decision::{DecisionProvider, DecisionRequest, Question};
use oz_steward::startlux::{StartLuxConfig, StartLuxProvider, StartLuxTier, StartLuxVariant};

#[tokio::test]
async fn live_startlux_systemone_returns_typed_answers() {
    if std::env::var("STARTLUX_LIVE").ok().as_deref() != Some("1") {
        eprintln!("skip: set STARTLUX_LIVE=1 with startlux-decide on :8001");
        return;
    }
    let endpoint =
        std::env::var("STARTLUX_ENDPOINT").unwrap_or_else(|_| "http://127.0.0.1:8001".into());
    let cfg = StartLuxConfig::new(StartLuxTier::B4, StartLuxVariant::AutoThink)
        .unwrap()
        .with_endpoint(endpoint)
        .with_timeout_ms(60_000);
    let provider = StartLuxProvider::new(cfg).unwrap();
    let health = provider.health().await.unwrap();
    assert_eq!(health["status"], "ok");
    assert_eq!(health["images"], true);

    let req = DecisionRequest::new(serde_json::json!({
        "goal": "fix the login page",
        "steps": [{"tool": "write", "target": "src/login.ts"}],
        "note": "one write inside src; tests not run yet"
    }))
    .ask(
        "verdict",
        Question::choice(
            "Is the worker on track?",
            ["on_track", "at_risk", "off_track", "stalled", "blocked"],
        ),
    )
    .ask(
        "evidence",
        Question::yes_no("Is there ledger evidence supporting the verdict?"),
    );
    let resp = provider.evaluate(&req).await.unwrap();
    eprintln!("live answers: {:?}", resp.answers);
    let verdict = resp.answers.get("verdict").expect("verdict answer");
    assert!(verdict.choice.is_some(), "typed choice missing");
    assert!(verdict.probabilities.len() >= 2, "probabilities missing");
    let evidence = resp.answers.get("evidence").expect("evidence answer");
    assert!(evidence.noul.is_some() || evidence.confidence.is_some());
}

#[tokio::test]
async fn live_startlux_accepts_an_image() {
    if std::env::var("STARTLUX_LIVE").ok().as_deref() != Some("1") {
        eprintln!("skip: set STARTLUX_LIVE=1 with startlux-decide on :8001");
        return;
    }
    let endpoint =
        std::env::var("STARTLUX_ENDPOINT").unwrap_or_else(|_| "http://127.0.0.1:8001".into());
    let cfg = StartLuxConfig::new(StartLuxTier::B4, StartLuxVariant::Instant)
        .unwrap()
        .with_endpoint(endpoint)
        .with_timeout_ms(60_000);
    let provider = StartLuxProvider::new(cfg).unwrap();
    let mut req = DecisionRequest::new(serde_json::json!({
        "goal": "check the attached image",
    }))
    .ask(
        "color",
        Question::choice(
            "What is the dominant color of the image?",
            ["red", "green", "blue", "white", "black"],
        ),
    );
    req.images = vec![
        "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAIAAAD8GO2jAAAAMElEQVR4nO3NMQEAIAzAsIF/zyCh175EQHve7LrL/TFIBskgGSSDZJAMkkEySAZTPqPEAT9XMVNHAAAAAElFTkSuQmCC".into(),
    ];
    let resp = provider.evaluate(&req).await.unwrap();
    eprintln!("live image answers: {:?}", resp.answers);
    let color = resp.answers.get("color").expect("color answer");
    assert!(color.choice.is_some());
}
