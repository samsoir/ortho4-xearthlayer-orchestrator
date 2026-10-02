use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

use crate::metadata::Metadata;
use crate::parameters::ProductionParameters;
use crate::policy::FailurePolicy;
use crate::raw::RawRegionSpec;
use crate::target::TargetLocation;
use crate::tile::TileId;
use crate::validate::{
    validate_failure_policy, validate_metadata, validate_parameters, validate_target,
    validate_tiles, ValidationReport,
};

/// A validated region specification.
///
/// Holding `TileId` rather than strings means every tile in a `RegionSpec`
/// is in range by construction, so the planner never revalidates.
///
/// Field order matters for TOML serialisation: `tiles` is an array value
/// and must be emitted before any table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegionSpec {
    pub tiles: BTreeSet<TileId>,
    pub metadata: Metadata,
    pub parameters: ProductionParameters,
    pub target: TargetLocation,
    pub failure_policy: FailurePolicy,
}

/// Why a specification could not be read.
#[derive(Debug)]
pub enum SpecError {
    /// The text was not well-formed TOML, or did not match the schema.
    Parse(toml::de::Error),
    /// The text parsed but broke one or more validation rules.
    Validation(ValidationReport),
}

impl fmt::Display for SpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => write!(f, "could not parse specification: {error}"),
            Self::Validation(report) => write!(f, "{report}"),
        }
    }
}

impl std::error::Error for SpecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Parse(error) => Some(error),
            Self::Validation(report) => Some(report),
        }
    }
}

impl RegionSpec {
    /// Validate a deserialized specification, collecting every fault.
    ///
    /// Each rule appends to one shared error list rather than returning
    /// early, which is what delivers the all-faults promise.
    pub fn validate(raw: RawRegionSpec) -> Result<Self, ValidationReport> {
        let mut errors = Vec::new();

        let tiles = validate_tiles(&raw.tiles, &mut errors);
        validate_parameters(&raw.parameters, &mut errors);
        validate_metadata(&raw.metadata, &mut errors);
        validate_target(&raw.target, &mut errors);
        validate_failure_policy(&raw.failure_policy, &mut errors);

        if errors.is_empty() {
            Ok(Self {
                tiles,
                metadata: raw.metadata,
                parameters: raw.parameters,
                target: raw.target,
                failure_policy: raw.failure_policy,
            })
        } else {
            Err(ValidationReport::new(errors))
        }
    }

    /// Parse and validate canonical TOML in one step.
    ///
    /// Note the asymmetry: a **schema** fault is reported alone, because
    /// serde stops at the first one, whereas every **validation** fault in a
    /// file that parses is reported together.
    pub fn from_toml(text: &str) -> Result<Self, SpecError> {
        let raw = RawRegionSpec::from_toml(text).map_err(SpecError::Parse)?;
        Self::validate(raw).map_err(SpecError::Validation)
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::*;

    /// Well formed TOML and schema, one validation fault: an empty tile set.
    const ONE_FAULT: &str = r#"
tiles = []

[metadata]
name = "North America"
region_code = "NA"
revision = 1

[parameters]
provider = "BI"
zoom = 16

[target]
root = "/srv/oxo/artifacts/NA"

[failure_policy]
max_attempts = 3
"#;

    #[test]
    fn a_parse_failure_names_itself_and_keeps_the_serde_error_as_its_cause() {
        let error = RegionSpec::from_toml("this is not toml").expect_err("reject");
        assert!(matches!(error, SpecError::Parse(_)), "got {error}");
        let rendered = error.to_string();
        assert!(
            rendered.contains("could not parse specification"),
            "{rendered}"
        );
        let source = error
            .source()
            .expect("a parse failure must carry its cause");
        assert!(!source.to_string().is_empty(), "empty cause");
    }

    #[test]
    fn a_validation_failure_renders_its_report_and_keeps_it_as_its_cause() {
        let error = RegionSpec::from_toml(ONE_FAULT).expect_err("reject");
        assert!(matches!(error, SpecError::Validation(_)), "got {error}");
        assert!(error.to_string().contains("tile set is empty"), "{error}");
        let source = error
            .source()
            .expect("a validation failure must carry its report");
        assert!(source.to_string().contains("tile set is empty"), "{source}");
    }
}
