use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Where produced artifacts are delivered.
///
/// Modelled as a struct rather than a bare path so that splitting ortho
/// and overlay destinations later is an added optional field rather than a
/// breaking change — that choice is still open in the design document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetLocation {
    /// Absolute path to the artifact root.
    pub root: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialises_a_target_root() {
        let target: TargetLocation =
            toml::from_str("root = \"/srv/oxo/artifacts/NA\"").expect("parse");
        assert_eq!(
            target.root,
            std::path::PathBuf::from("/srv/oxo/artifacts/NA")
        );
    }
}
