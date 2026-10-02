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
    pub fn from_toml(text: &str) -> Result<Self, SpecError> {
        let raw = RawRegionSpec::from_toml(text).map_err(SpecError::Parse)?;
        Self::validate(raw).map_err(SpecError::Validation)
    }
}
