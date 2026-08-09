#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationStatus {
    Pending,
    UnderReview,
    Verified,
    Rejected,
    Expired,
}

#[derive(Debug, Clone)]
pub struct Conjecture {
    pub id: u64,
    pub node_a: u64,
    pub node_b: u64,
    pub statement: String,
    pub sss_score: f32,
    pub verification_status: VerificationStatus,
}

#[derive(Debug, Clone)]
pub struct SSSScore {
    pub simplicity: f32,
    pub surprise: f32,
    pub composite: f32,
}

impl SSSScore {
    pub fn is_interesting(&self) -> bool {
        self.composite > 0.5
    }
}

#[derive(Debug, Clone)]
pub struct RamblingConfig {
    pub idle_threshold_secs: u64,
    pub max_hops: usize,
    pub cpu_time_limit_ms: u64,
    pub max_conjectures: usize,
    pub decay_interval_hours: u64,
}

impl Default for RamblingConfig {
    fn default() -> Self {
        Self {
            idle_threshold_secs: 5,
            max_hops: 3,
            cpu_time_limit_ms: 2000,
            max_conjectures: 3,
            decay_interval_hours: 24,
        }
    }
}
