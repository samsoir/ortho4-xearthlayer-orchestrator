use serde::{Deserialize, Serialize};

/// What to do when a tile fails.
///
/// Stated here because the specification carries intent; enforced by the
/// job server, which is the only component positioned to act on it.
///
/// Field order matters for TOML serialisation: scalars before the array.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailurePolicy {
    /// Total attempts allowed per tile, including the first. At least 1.
    pub max_attempts: u32,
    /// Fixed delay between attempts. Unsigned, so the design document's
    /// "non-negative backoff" rule is enforced by the type and needs no
    /// validation rule.
    #[serde(default)]
    pub backoff_seconds: u64,
    /// Opaque alert destinations. Their representation is an open decision
    /// owned by the observability sub-project.
    #[serde(default)]
    pub alert_destinations: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialises_a_failure_policy() {
        let text = "max_attempts = 3\nbackoff_seconds = 60\nalert_destinations = [\"ops\"]\n";
        let policy: FailurePolicy = toml::from_str(text).expect("parse");
        assert_eq!(policy.max_attempts, 3);
        assert_eq!(policy.backoff_seconds, 60);
        assert_eq!(policy.alert_destinations, vec!["ops".to_string()]);
    }

    #[test]
    fn backoff_and_alerts_default_when_absent() {
        let policy: FailurePolicy = toml::from_str("max_attempts = 1\n").expect("parse");
        assert_eq!(policy.backoff_seconds, 0);
        assert!(policy.alert_destinations.is_empty());
    }
}
