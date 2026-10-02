use serde::{Deserialize, Serialize};

/// Longest accepted region code.
///
/// Shared with the message that reports a rejection, so the bound and the
/// text that explains it cannot drift apart, and so the later API and web
/// interface enforce this bound rather than re-deriving one.
pub const REGION_CODE_MAX_LEN: usize = 16;

/// Identifying information for a region.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    /// Human-readable region name.
    pub name: String,
    /// Short region code, e.g. `NA` or `EU-1`.
    pub region_code: String,
    /// Operator-set revision, starting at 1.
    pub revision: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialises_metadata() {
        let text = "name = \"North America\"\nregion_code = \"NA\"\nrevision = 1\n";
        let metadata: Metadata = toml::from_str(text).expect("parse");
        assert_eq!(metadata.name, "North America");
        assert_eq!(metadata.region_code, "NA");
        assert_eq!(metadata.revision, 1);
    }
}
