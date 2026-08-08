//! Rate limiting middleware — token bucket per session.
//!
//! Limits API calls to a configurable rate per session to prevent abuse.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

const DEFAULT_RATE: u32 = 60; // requests per window
const DEFAULT_WINDOW: Duration = Duration::from_secs(60);
const MAX_BUCKETS: usize = 1000;

struct Bucket {
    tokens: u32,
    max_tokens: u32,
    refill_rate: f64, // tokens per second
    last_refill: Instant,
}

impl Bucket {
    fn new(max_tokens: u32, window: Duration) -> Self {
        Bucket {
            tokens: max_tokens,
            max_tokens,
            refill_rate: max_tokens as f64 / window.as_secs_f64(),
            last_refill: Instant::now(),
        }
    }

    fn try_consume(&mut self) -> bool {
        self.refill();
        if self.tokens > 0 {
            self.tokens -= 1;
            true
        } else {
            false
        }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        let new_tokens = (elapsed * self.refill_rate) as u32;
        self.tokens = (self.tokens + new_tokens).min(self.max_tokens);
        self.last_refill = now;
    }
}

#[derive(Clone)]
pub struct RateLimiter {
    buckets: Arc<Mutex<HashMap<String, Bucket>>>,
    max_tokens: u32,
    window: Duration,
}

impl RateLimiter {
    pub fn new(max_tokens: u32, window_secs: u64) -> Self {
        RateLimiter {
            buckets: Arc::new(Mutex::new(HashMap::new())),
            max_tokens,
            window: Duration::from_secs(window_secs),
        }
    }

    fn check(&self, key: &str) -> bool {
        let mut map = self.buckets.lock().unwrap();
        if map.len() > MAX_BUCKETS {
            map.clear();
        }
        map.entry(key.to_string())
            .or_insert_with(|| Bucket::new(self.max_tokens, self.window))
            .try_consume()
    }
}

pub async fn rate_limit(
    req: Request,
    next: Next,
) -> Response {
    // Extract session identifier: prefer session_id from path, fallback to IP
    let key = req.uri().path()
        .split('/')
        .nth(3) // /api/sessions/:id/...
        .filter(|s| !s.is_empty())
        .map(|s| format!("session:{s}"))
        .unwrap_or_else(|| {
            let ip = req.headers()
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown");
            format!("ip:{ip}")
        });

    let limiter = req.extensions()
        .get::<RateLimiter>()
        .cloned()
        .unwrap_or_else(|| RateLimiter::new(DEFAULT_RATE, DEFAULT_WINDOW.as_secs()));

    if limiter.check(&key) {
        next.run(req).await
    } else {
        let body = serde_json::json!({
            "error": "rate_limit_exceeded",
            "message": "Too many requests. Please wait and try again.",
            "retry_after_secs": DEFAULT_WINDOW.as_secs(),
        });
        (StatusCode::TOO_MANY_REQUESTS, axum::Json(body)).into_response()
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        RateLimiter::new(DEFAULT_RATE, DEFAULT_WINDOW.as_secs())
    }
}
