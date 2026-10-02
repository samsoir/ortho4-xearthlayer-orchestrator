use serde::Deserialize;

use crate::metadata::Metadata;
use crate::parameters::ProductionParameters;
use crate::policy::FailurePolicy;
use crate::target::TargetLocation;

/// A specification as deserialized, before validation.
///
/// Tiles are strings here on purpose. Validation promises to report every
/// fault in a specification, and a `Vec<TileId>` could not hold a
/// malformed identifier long enough to report it — deserialization would
/// reject the first one and the operator would fix faults one run at a
/// time.
///
/// Field order matters for TOML: `tiles` is a top-level array and must be
/// emitted before any table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRegionSpec {
    pub tiles: Vec<String>,
    pub metadata: Metadata,
    pub parameters: ProductionParameters,
    pub target: TargetLocation,
    pub failure_policy: FailurePolicy,
}

impl RawRegionSpec {
    /// Deserialize from the canonical TOML form.
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }
}

#[cfg(test)]
mod tests {
    use super::RawRegionSpec;

    const VALID: &str = r#"
tiles = ["+50-002", "+51-002"]

[metadata]
name = "North America"
region_code = "NA"
revision = 1

[parameters]
provider = "BI"
zoom = 16
include_overlays = true

[parameters.raw]
cover_airports_with_highres = "ICAO"

[target]
root = "/srv/oxo/artifacts/NA"

[failure_policy]
max_attempts = 3
backoff_seconds = 60
alert_destinations = ["ops"]
"#;

    #[test]
    fn parses_a_complete_specification() {
        let raw = RawRegionSpec::from_toml(VALID).expect("parse");
        assert_eq!(raw.tiles, vec!["+50-002", "+51-002"]);
        assert_eq!(raw.metadata.region_code, "NA");
        assert_eq!(raw.parameters.zoom, 16);
        assert_eq!(raw.failure_policy.max_attempts, 3);
    }

    #[test]
    fn keeps_malformed_tiles_as_strings_for_validation_to_report() {
        let text = VALID.replace("\"+51-002\"", "\"nope\"");
        let raw =
            RawRegionSpec::from_toml(&text).expect("deserialisation must not reject a bad tile");
        assert_eq!(raw.tiles, vec!["+50-002", "nope"]);
    }

    #[test]
    fn rejects_an_unknown_section() {
        let text = format!("{VALID}\n[nonsense]\nkey = 1\n");
        let error = RawRegionSpec::from_toml(&text).expect_err("should reject");
        assert!(error.to_string().contains("nonsense"), "{error}");
    }
}
