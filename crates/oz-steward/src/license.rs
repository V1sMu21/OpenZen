//! StartLux weight licensing gate.
//!
//! Code is Apache-2.0, weights are CC BY-NC 4.0. OpenZen is MIT: it must not
//! bundle the weights or silently download them for a commercial user.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseCase {
    Personal,
    Commercial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LicenseStatus {
    Allowed,
    NeedsAcknowledgement,
    NeedsCommercialLicense,
    Blocked,
}

/// `acknowledged_noncommercial` is the user's explicit checkbox; it cannot be
/// defaulted to true by the app. `has_commercial_license` is a separate,
/// explicit entitlement.
pub fn check(
    use_case: UseCase,
    acknowledged_noncommercial: bool,
    has_commercial_license: bool,
) -> LicenseStatus {
    match use_case {
        UseCase::Personal => {
            if acknowledged_noncommercial {
                LicenseStatus::Allowed
            } else {
                LicenseStatus::NeedsAcknowledgement
            }
        }
        UseCase::Commercial => {
            if has_commercial_license {
                LicenseStatus::Allowed
            } else {
                LicenseStatus::NeedsCommercialLicense
            }
        }
    }
}

pub fn can_download(use_case: UseCase, acknowledged: bool, commercial: bool) -> bool {
    check(use_case, acknowledged, commercial) == LicenseStatus::Allowed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personal_requires_acknowledgement() {
        assert_eq!(
            check(UseCase::Personal, false, false),
            LicenseStatus::NeedsAcknowledgement
        );
        assert_eq!(
            check(UseCase::Personal, true, false),
            LicenseStatus::Allowed
        );
    }

    #[test]
    fn commercial_requires_license_even_with_acknowledgement() {
        assert_eq!(
            check(UseCase::Commercial, true, false),
            LicenseStatus::NeedsCommercialLicense
        );
        assert_eq!(
            check(UseCase::Commercial, true, true),
            LicenseStatus::Allowed
        );
        assert!(!can_download(UseCase::Commercial, true, false));
    }
}
