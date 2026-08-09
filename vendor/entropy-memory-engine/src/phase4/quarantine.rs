use std::sync::{Arc, RwLock};

use crate::core::types::{MemoryContent, MemoryInput, Query};
use crate::core::MemoryResult;
use crate::l2::L2Engine;
use crate::phase2::types::Conjecture;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineStatus {
    Pending,
    UnderReview,
    Verified,
    Rejected,
    Expired,
}

#[derive(Debug, Clone)]
pub struct QuarantinedConjecture {
    pub conjecture: Conjecture,
    pub created_at: i64,
    pub verification_attempts: u32,
    pub evidence_for: Vec<u64>,
    pub evidence_against: Vec<u64>,
    pub status: QuarantineStatus,
}

#[derive(Debug, Clone)]
pub struct QuarantineConfig {
    pub max_quarantine_size: usize,
    pub verification_threshold: u32,
    pub expiry_hours: u64,
    pub auto_verify_interval_secs: u64,
}

impl Default for QuarantineConfig {
    fn default() -> Self {
        Self {
            max_quarantine_size: 500,
            verification_threshold: 3,
            expiry_hours: 168,
            auto_verify_interval_secs: 3600,
        }
    }
}

pub struct QuarantineManager {
    conjectures: Arc<RwLock<Vec<QuarantinedConjecture>>>,
    l2_engine: Arc<L2Engine>,
    config: QuarantineConfig,
}

impl QuarantineManager {
    pub fn new(l2_engine: Arc<L2Engine>, config: QuarantineConfig) -> Self {
        Self {
            conjectures: Arc::new(RwLock::new(Vec::new())),
            l2_engine,
            config,
        }
    }

    pub fn admit(&self, conjecture: Conjecture) -> bool {
        let mut conjectures = self.conjectures.write().unwrap();

        if conjectures.len() >= self.config.max_quarantine_size {
            self.cleanup_expired_internal(&mut conjectures);
            if conjectures.len() >= self.config.max_quarantine_size {
                return false;
            }
        }

        let qc = QuarantinedConjecture {
            conjecture,
            created_at: crate::core::now_nanos(),
            verification_attempts: 0,
            evidence_for: Vec::new(),
            evidence_against: Vec::new(),
            status: QuarantineStatus::Pending,
        };

        conjectures.push(qc);
        true
    }

    pub fn verify_cycle(&self) {
        let pending: Vec<QuarantinedConjecture> = {
            let conjectures = self.conjectures.read().unwrap();
            conjectures
                .iter()
                .filter(|qc| {
                    qc.status == QuarantineStatus::Pending
                        || qc.status == QuarantineStatus::UnderReview
                })
                .cloned()
                .collect()
        };

        if pending.is_empty() {
            return;
        }

        let mut updated = Vec::new();
        let now = crate::core::now_nanos();

        for mut qc in pending {
            let expiry_ns = (self.config.expiry_hours as i64)
                .saturating_mul(3600)
                .saturating_mul(1_000_000_000);
            if now - qc.created_at > expiry_ns {
                qc.status = QuarantineStatus::Expired;
                updated.push(qc);
                continue;
            }

            qc.status = QuarantineStatus::UnderReview;

            let text = &qc.conjecture.statement;
            let query = Query::by_text(text);
            let results = self.l2_engine.search_semantic(&query, 5);

            let mut evidence_count = 0u32;
            for (id, dist) in &results {
                if *dist < 0.5 {
                    if !qc.evidence_for.contains(id) {
                        qc.evidence_for.push(*id);
                    }
                    evidence_count += 1;
                } else {
                    if !qc.evidence_against.contains(id) {
                        qc.evidence_against.push(*id);
                    }
                }
            }

            if evidence_count > 0 {
                qc.verification_attempts += 1;
            }

            if qc.verification_attempts >= self.config.verification_threshold {
                qc.status = QuarantineStatus::Verified;
            } else {
                qc.status = QuarantineStatus::Pending;
            }

            if qc.status == QuarantineStatus::Expired
                || qc.status == QuarantineStatus::Verified
                || qc.verification_attempts > 0
            {
                updated.push(qc);
            }
        }

        let mut conjectures = self.conjectures.write().unwrap();
        for updated_qc in &updated {
            if let Some(existing) = conjectures
                .iter_mut()
                .find(|c| c.conjecture.id == updated_qc.conjecture.id)
            {
                *existing = updated_qc.clone();
            }
        }
    }

    pub fn promote(&self, conjecture_id: u64) -> MemoryResult<Option<MemoryInput>> {
        let mut conjectures = self.conjectures.write().unwrap();

        if let Some(pos) = conjectures
            .iter()
            .position(|c| c.conjecture.id == conjecture_id)
        {
            if conjectures[pos].status == QuarantineStatus::Verified
                || conjectures[pos].status == QuarantineStatus::Pending
            {
                let qc = &conjectures[pos];
                let input =
                    MemoryInput::new(MemoryContent::Summary(qc.conjecture.statement.clone()))
                        .with_importance(0.5);
                conjectures[pos].status = QuarantineStatus::Verified;
                return Ok(Some(input));
            }
        }
        Ok(None)
    }

    pub fn cleanup_expired(&self) {
        let mut conjectures = self.conjectures.write().unwrap();
        self.cleanup_expired_internal(&mut conjectures);
    }

    fn cleanup_expired_internal(&self, conjectures: &mut Vec<QuarantinedConjecture>) {
        let now = crate::core::now_nanos();
        let expiry_ns = (self.config.expiry_hours as i64)
            .saturating_mul(3600)
            .saturating_mul(1_000_000_000);
        conjectures.retain(|c| {
            c.status != QuarantineStatus::Expired
                && c.status != QuarantineStatus::Rejected
                && (now - c.created_at) <= expiry_ns
        });
    }

    pub fn stats(&self) -> QuarantineStats {
        let conjectures = self.conjectures.read().unwrap();
        let total = conjectures.len();
        let verified = conjectures
            .iter()
            .filter(|c| c.status == QuarantineStatus::Verified)
            .count();
        let rejected = conjectures
            .iter()
            .filter(|c| c.status == QuarantineStatus::Rejected)
            .count();
        let pending = total - verified - rejected;
        QuarantineStats {
            total,
            verified,
            rejected,
            pending,
        }
    }

    pub fn config(&self) -> &QuarantineConfig {
        &self.config
    }

    pub fn all_conjectures(&self) -> Vec<QuarantinedConjecture> {
        self.conjectures.read().unwrap().clone()
    }
}

#[derive(Debug, Clone, Default)]
pub struct QuarantineStats {
    pub total: usize,
    pub verified: usize,
    pub rejected: usize,
    pub pending: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::{Fact, MemoryContent, MemoryInput};
    use crate::l2::{HnswConfig, L2Config, L2Engine};
    use crate::phase2::types::Conjecture;
    use crate::phase2::VerificationStatus;

    fn make_l2() -> Arc<L2Engine> {
        Arc::new(L2Engine::new(L2Config {
            hnsw: HnswConfig {
                dimension: 16,
                ..Default::default()
            },
            dimension: 16,
            ..Default::default()
        }))
    }

    fn make_conjecture(id: u64, statement: &str) -> Conjecture {
        Conjecture {
            id,
            node_a: 1,
            node_b: 2,
            statement: statement.into(),
            sss_score: 0.7,
            verification_status: VerificationStatus::Pending,
        }
    }

    #[test]
    fn test_admit_conjecture() {
        let l2 = make_l2();
        let qm = QuarantineManager::new(l2, QuarantineConfig::default());
        let c = make_conjecture(100, "A and B may be related");
        assert!(qm.admit(c));
        let stats = qm.stats();
        assert_eq!(stats.total, 1);
    }

    #[test]
    fn test_verify_cycle() {
        let l2 = make_l2();
        l2.insert(MemoryInput::new(MemoryContent::Fact(Fact::new(
            "A", "connects", "B",
        ))))
        .unwrap();

        let qm = QuarantineManager::new(l2, QuarantineConfig::default());
        let c = make_conjecture(101, "A connects B");
        qm.admit(c);

        qm.verify_cycle();
        let conjectures = qm.all_conjectures();
        assert!(!conjectures.is_empty());
    }

    #[test]
    fn test_promote_conjecture() {
        let l2 = make_l2();
        let qm = QuarantineManager::new(
            l2,
            QuarantineConfig {
                verification_threshold: 0,
                ..Default::default()
            },
        );
        let c = make_conjecture(102, "test promotion");
        qm.admit(c);

        let result = qm.promote(102);
        assert!(result.is_ok());
        assert!(result.unwrap().is_some());
    }

    #[test]
    fn test_cleanup_expired() {
        let l2 = make_l2();
        let qm = QuarantineManager::new(
            l2,
            QuarantineConfig {
                expiry_hours: 0,
                ..Default::default()
            },
        );
        let c = make_conjecture(103, "expired");
        qm.admit(c);

        {
            let mut conjectures = qm.conjectures.write().unwrap();
            if let Some(qc) = conjectures.first_mut() {
                qc.created_at = 0;
            }
        }

        qm.cleanup_expired();
        let stats = qm.stats();
        assert_eq!(stats.total, 0, "all should be cleaned up when created_at=0");
    }

    #[test]
    fn test_max_capacity() {
        let l2 = make_l2();
        let qm = QuarantineManager::new(
            l2,
            QuarantineConfig {
                max_quarantine_size: 2,
                ..Default::default()
            },
        );

        assert!(qm.admit(make_conjecture(1, "first")));
        assert!(qm.admit(make_conjecture(2, "second")));
        assert!(!qm.admit(make_conjecture(3, "rejected")));

        let stats = qm.stats();
        assert_eq!(stats.total, 2);
    }

    #[test]
    fn test_stats_counts() {
        let l2 = make_l2();
        let qm = QuarantineManager::new(l2, QuarantineConfig::default());
        qm.admit(make_conjecture(1, "one"));
        qm.admit(make_conjecture(2, "two"));
        let stats = qm.stats();
        assert_eq!(stats.total, 2);
        assert_eq!(stats.pending, 2);
        assert_eq!(stats.verified, 0);
    }
}
