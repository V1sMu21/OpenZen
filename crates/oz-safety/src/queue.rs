//! Concurrent approval queue — prevents overlapping approval dialogs.
//!
//! When multiple tools in a parallel dispatch need approval,
//! they are queued and presented to the user one at a time.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::approval::ApprovalRequest;

#[derive(Clone)]
pub struct ApprovalQueue {
    inner: Arc<Mutex<VecDeque<PendingApproval>>>,
}

struct PendingApproval {
    request: ApprovalRequest,
    resolved: bool,
}

impl ApprovalQueue {
    pub fn new() -> Self {
        ApprovalQueue {
            inner: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub fn push(&self, request: ApprovalRequest) {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(PendingApproval {
                request,
                resolved: false,
            });
    }

    pub fn pop(&self) -> Option<ApprovalRequest> {
        let mut q = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pos) = q.iter().position(|p| !p.resolved) {
            q[pos].resolved = true;
            Some(q[pos].request.clone())
        } else {
            None
        }
    }

    pub fn pending_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .filter(|p| !p.resolved)
            .count()
    }

    pub fn clear(&self) {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    pub fn current(&self) -> Option<ApprovalRequest> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .find(|p| !p.resolved)
            .map(|p| p.request.clone())
    }
}

impl Default for ApprovalQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust::ApprovalInfo;
    use crate::trust::TrustLevel;

    fn make_request(id: &str) -> ApprovalRequest {
        ApprovalRequest {
            session_id: "test".into(),
            tool_name: id.into(),
            pattern: "test".into(),
            arguments: serde_json::json!({}),
            info: ApprovalInfo {
                tool_name: id.into(),
                pattern: "test".into(),
                arguments_summary: String::new(),
                approved_count: 0,
                current_level: TrustLevel::AlwaysAsk,
            },
        }
    }

    #[test]
    fn test_queue_ordering() {
        let q = ApprovalQueue::new();
        q.push(make_request("a"));
        q.push(make_request("b"));
        q.push(make_request("c"));

        assert_eq!(q.pending_count(), 3);
        assert_eq!(q.pop().unwrap().tool_name, "a");
        assert_eq!(q.pending_count(), 2);
        assert_eq!(q.pop().unwrap().tool_name, "b");
        assert_eq!(q.pop().unwrap().tool_name, "c");
        assert_eq!(q.pending_count(), 0);
    }

    #[test]
    fn test_clear() {
        let q = ApprovalQueue::new();
        q.push(make_request("a"));
        q.push(make_request("b"));
        q.clear();
        assert_eq!(q.pending_count(), 0);
    }
}
