//! Rule test for the post-completion review gate.
//!
//! The expensive 2026-10-23 bug was `!v.pass` being treated as "must fix":
//! an inconclusive reviewer sent the run back to work forever. The rule under
//! test is the fix: only a *named defect* earns another round.

use oz_core::quality::{review_requires_fix, ReviewIssue, ReviewStatus, ReviewVerdict};

fn verdict(status: ReviewStatus, issues: Vec<ReviewIssue>, pass: bool) -> ReviewVerdict {
    ReviewVerdict {
        pass,
        issues,
        status,
        derived_from_issues: false,
    }
}

#[test]
fn only_a_named_defect_earns_another_fix_round() {
    let failed = verdict(ReviewStatus::Failed, vec![], false);
    assert!(
        review_requires_fix(&failed),
        "a failed review must be fixed"
    );

    let high = verdict(
        ReviewStatus::Passed,
        vec![ReviewIssue {
            severity: "high".into(),
            item: "missing null check".into(),
        }],
        true,
    );
    assert!(
        review_requires_fix(&high),
        "a high-severity defect must be fixed"
    );

    let inconclusive = verdict(ReviewStatus::InsufficientEvidence, vec![], false);
    assert!(
        !review_requires_fix(&inconclusive),
        "an inconclusive review must end with an honest 'unverified' note, not another round"
    );

    let unavailable = verdict(ReviewStatus::Unavailable, vec![], false);
    assert!(
        !review_requires_fix(&unavailable),
        "a transport failure is not a defect"
    );

    let low = verdict(
        ReviewStatus::Passed,
        vec![ReviewIssue {
            severity: "low".into(),
            item: "naming".into(),
        }],
        true,
    );
    assert!(
        !review_requires_fix(&low),
        "a low-severity note must not gate the run"
    );
}
