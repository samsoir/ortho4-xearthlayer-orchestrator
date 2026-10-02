use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::metadata::Metadata;
use crate::parameters::{ProductionParameters, RESERVED_RAW_KEYS, ZOOM_MAX, ZOOM_MIN};
use crate::policy::FailurePolicy;
use crate::target::TargetLocation;
use crate::tile::{TileId, TileIdParseError};

/// A single static-validation fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    EmptyTileSet,
    InvalidTileId {
        value: String,
        reason: TileIdParseError,
    },
    DuplicateTile {
        value: String,
        occurrences: usize,
    },
    ZoomOutOfRange {
        zoom: u8,
    },
    EmptyProviderCode,
    InvalidProviderCode {
        value: String,
    },
    ReservedRawKey {
        key: String,
        curated_field: &'static str,
    },
    EmptyName,
    EmptyRegionCode,
    InvalidRegionCode {
        value: String,
    },
    RevisionTooLow,
    EmptyTargetRoot,
    TargetRootNotAbsolute {
        value: String,
    },
    MaxAttemptsTooLow,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyTileSet => {
                write!(f, "tile set is empty: a region must name at least one tile")
            }
            Self::InvalidTileId { value, reason } => {
                write!(f, "tile {value:?} is not a valid identifier: {reason}")
            }
            Self::DuplicateTile { value, occurrences } => write!(
                f,
                "tile {value} is listed {occurrences} times; list each tile once"
            ),
            Self::ZoomOutOfRange { zoom } => {
                write!(f, "zoom level {zoom} is outside {ZOOM_MIN}..={ZOOM_MAX}")
            }
            Self::EmptyProviderCode => write!(f, "provider code is empty"),
            Self::InvalidProviderCode { value } => write!(
                f,
                "provider code {value:?} is malformed: expected at most 64 \
                 characters with no whitespace, control characters or path \
                 separators"
            ),
            Self::ReservedRawKey { key, curated_field } => write!(
                f,
                "raw override {key:?} is owned by the curated field \
                 {curated_field:?}; set {curated_field} instead of \
                 shadowing it"
            ),
            Self::EmptyName => write!(f, "region name is empty"),
            Self::EmptyRegionCode => write!(f, "region code is empty"),
            Self::InvalidRegionCode { value } => write!(
                f,
                "region code {value:?} is malformed: expected at most 16 \
                 characters of A-Z, 0-9 and '-'"
            ),
            Self::RevisionTooLow => write!(f, "revision must be at least 1"),
            Self::EmptyTargetRoot => write!(f, "target root is empty"),
            Self::TargetRootNotAbsolute { value } => {
                write!(f, "target root {value:?} must be an absolute path")
            }
            Self::MaxAttemptsTooLow => {
                write!(f, "failure policy max_attempts must be at least 1")
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// Every fault found in one specification.
///
/// Validation collects rather than short-circuits: an operator correcting
/// a specification that names thousands of tiles cannot be made to re-run
/// once per mistake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationReport {
    errors: Vec<ValidationError>,
}

impl ValidationReport {
    pub fn new(errors: Vec<ValidationError>) -> Self {
        Self { errors }
    }

    pub fn errors(&self) -> &[ValidationError] {
        &self.errors
    }

    pub fn len(&self) -> usize {
        self.errors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }
}

impl fmt::Display for ValidationReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for error in &self.errors {
            if !first {
                writeln!(f)?;
            }
            write!(f, "{error}")?;
            first = false;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationReport {}

/// Validate the raw tile list, returning the tiles that parsed.
///
/// Counting distinct values first means a malformed identifier appearing
/// twice yields one `InvalidTileId` and one `DuplicateTile`, rather than
/// two of the former. Iterating a `BTreeMap` also makes fault order
/// deterministic, which matters for testing and for diffable output.
#[allow(dead_code)]
pub(crate) fn validate_tiles(
    raw: &[String],
    errors: &mut Vec<ValidationError>,
) -> BTreeSet<TileId> {
    if raw.is_empty() {
        errors.push(ValidationError::EmptyTileSet);
    }

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for value in raw {
        *counts.entry(value.as_str()).or_insert(0) += 1;
    }

    let mut tiles = BTreeSet::new();
    for (value, occurrences) in counts {
        if occurrences > 1 {
            errors.push(ValidationError::DuplicateTile {
                value: value.to_string(),
                occurrences,
            });
        }
        match value.parse::<TileId>() {
            Ok(tile) => {
                tiles.insert(tile);
            }
            Err(reason) => errors.push(ValidationError::InvalidTileId {
                value: value.to_string(),
                reason,
            }),
        }
    }

    tiles
}

/// Validate production parameters.
///
/// Provider codes are checked for shape only. Whether a code exists is a
/// property of an Ortho4XP installation — codes are `.lay` filenames under
/// `Providers/` — and so is whether that provider permits the requested
/// zoom. Both are environmental validation, owned by the control plane.
#[allow(dead_code)]
pub(crate) fn validate_parameters(
    parameters: &ProductionParameters,
    errors: &mut Vec<ValidationError>,
) {
    if parameters.provider.is_empty() {
        errors.push(ValidationError::EmptyProviderCode);
    } else if !is_well_formed_provider_code(&parameters.provider) {
        errors.push(ValidationError::InvalidProviderCode {
            value: parameters.provider.clone(),
        });
    }

    if !(ZOOM_MIN..=ZOOM_MAX).contains(&parameters.zoom) {
        errors.push(ValidationError::ZoomOutOfRange {
            zoom: parameters.zoom,
        });
    }

    for &(key, curated_field) in RESERVED_RAW_KEYS {
        if parameters.raw.contains_key(key) {
            errors.push(ValidationError::ReservedRawKey {
                key: key.to_string(),
                curated_field,
            });
        }
    }
}

fn is_well_formed_provider_code(code: &str) -> bool {
    code.len() <= 64
        && !code.contains('/')
        && !code.contains('\\')
        && code.chars().all(|c| !c.is_whitespace() && !c.is_control())
}

/// Validate region metadata.
#[allow(dead_code)]
pub(crate) fn validate_metadata(metadata: &Metadata, errors: &mut Vec<ValidationError>) {
    if metadata.name.trim().is_empty() {
        errors.push(ValidationError::EmptyName);
    }

    if metadata.region_code.is_empty() {
        errors.push(ValidationError::EmptyRegionCode);
    } else if !is_well_formed_region_code(&metadata.region_code) {
        errors.push(ValidationError::InvalidRegionCode {
            value: metadata.region_code.clone(),
        });
    }

    if metadata.revision < 1 {
        errors.push(ValidationError::RevisionTooLow);
    }
}

fn is_well_formed_region_code(code: &str) -> bool {
    code.len() <= 16
        && code
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-')
}

/// Validate the artifact target.
///
/// The root must be absolute: a relative path means something different
/// depending on where a pod happens to start, which is exactly the
/// ambiguity a specification exists to remove. Whether the path exists or
/// is writable is environmental validation.
#[allow(dead_code)]
pub(crate) fn validate_target(target: &TargetLocation, errors: &mut Vec<ValidationError>) {
    if target.root.as_os_str().is_empty() {
        errors.push(ValidationError::EmptyTargetRoot);
    } else if !target.root.is_absolute() {
        errors.push(ValidationError::TargetRootNotAbsolute {
            value: target.root.display().to_string(),
        });
    }
}

/// Validate the failure policy.
///
/// Only `max_attempts` needs a rule: `backoff_seconds` is unsigned, so the
/// design document's "non-negative backoff" requirement is enforced by the
/// type, and alert destinations are opaque until the observability
/// sub-project decides their representation.
#[allow(dead_code)]
pub(crate) fn validate_failure_policy(policy: &FailurePolicy, errors: &mut Vec<ValidationError>) {
    if policy.max_attempts < 1 {
        errors.push(ValidationError::MaxAttemptsTooLow);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_lists_every_fault_one_per_line() {
        let report = ValidationReport::new(vec![
            ValidationError::EmptyTileSet,
            ValidationError::ZoomOutOfRange { zoom: 99 },
        ]);
        assert_eq!(report.len(), 2);
        assert!(!report.is_empty());
        let rendered = report.to_string();
        assert_eq!(rendered.lines().count(), 2);
        assert!(rendered.contains("tile set is empty"), "{rendered}");
        assert!(rendered.contains("zoom level 99"), "{rendered}");
    }

    #[test]
    fn an_empty_report_is_empty() {
        let report = ValidationReport::new(Vec::new());
        assert!(report.is_empty());
        assert_eq!(report.len(), 0);
    }

    #[test]
    fn a_reserved_raw_key_names_the_field_that_owns_it() {
        let error = ValidationError::ReservedRawKey {
            key: "default_zl".to_string(),
            curated_field: "zoom",
        };
        let rendered = error.to_string();
        assert!(rendered.contains("default_zl"), "{rendered}");
        assert!(rendered.contains("zoom"), "{rendered}");
    }

    #[test]
    fn an_empty_tile_list_is_a_fault() {
        let mut errors = Vec::new();
        let tiles = validate_tiles(&[], &mut errors);
        assert!(tiles.is_empty());
        assert_eq!(errors, vec![ValidationError::EmptyTileSet]);
    }

    #[test]
    fn good_tiles_are_collected_and_ordered() {
        let mut errors = Vec::new();
        let input = vec!["+51-002".to_string(), "+50-002".to_string()];
        let tiles = validate_tiles(&input, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
        let ordered: Vec<String> = tiles.iter().map(ToString::to_string).collect();
        assert_eq!(ordered, vec!["+50-002", "+51-002"]);
    }

    #[test]
    fn a_duplicate_tile_is_a_fault_and_is_not_silently_collapsed() {
        let mut errors = Vec::new();
        let input = vec!["+50-002".to_string(), "+50-002".to_string()];
        let tiles = validate_tiles(&input, &mut errors);
        assert_eq!(tiles.len(), 1);
        assert_eq!(
            errors,
            vec![ValidationError::DuplicateTile {
                value: "+50-002".to_string(),
                occurrences: 2,
            }]
        );
    }

    #[test]
    fn a_malformed_tile_is_reported_once_per_distinct_value() {
        let mut errors = Vec::new();
        let input = vec![
            "nope".to_string(),
            "nope".to_string(),
            "+91+000".to_string(),
        ];
        let tiles = validate_tiles(&input, &mut errors);
        assert!(tiles.is_empty());
        let invalid: Vec<&ValidationError> = errors
            .iter()
            .filter(|e| matches!(e, ValidationError::InvalidTileId { .. }))
            .collect();
        assert_eq!(invalid.len(), 2, "{errors:?}");
    }

    #[test]
    fn faults_are_ordered_and_a_tile_can_be_both_malformed_and_duplicated() {
        let mut errors = Vec::new();
        let input = vec![
            "nope".to_string(),
            "nope".to_string(),
            "+50-002".to_string(),
            "bad".to_string(),
        ];
        let tiles = validate_tiles(&input, &mut errors);

        // The one well-formed tile still lands in the returned set.
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles.iter().next().unwrap().to_string(), "+50-002");

        // Faults come back in deterministic order (BTreeMap over distinct
        // values), and "nope" yields ONE DuplicateTile plus ONE
        // InvalidTileId rather than two of either.
        assert_eq!(
            errors,
            vec![
                ValidationError::InvalidTileId {
                    value: "bad".to_string(),
                    reason: TileIdParseError::WrongLength { got: 3 },
                },
                ValidationError::DuplicateTile {
                    value: "nope".to_string(),
                    occurrences: 2,
                },
                ValidationError::InvalidTileId {
                    value: "nope".to_string(),
                    reason: TileIdParseError::WrongLength { got: 4 },
                },
            ]
        );
    }

    fn parameters() -> ProductionParameters {
        ProductionParameters {
            provider: "BI".to_string(),
            zoom: 16,
            include_overlays: false,
            raw: BTreeMap::new(),
        }
    }

    #[test]
    fn well_formed_parameters_pass() {
        let mut errors = Vec::new();
        validate_parameters(&parameters(), &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn an_empty_provider_code_is_a_fault() {
        let mut errors = Vec::new();
        let mut p = parameters();
        p.provider = String::new();
        validate_parameters(&p, &mut errors);
        assert_eq!(errors, vec![ValidationError::EmptyProviderCode]);
    }

    #[test]
    fn a_provider_code_with_a_path_separator_is_a_fault() {
        let mut errors = Vec::new();
        let mut p = parameters();
        p.provider = "Global/BI".to_string();
        validate_parameters(&p, &mut errors);
        assert_eq!(
            errors,
            vec![ValidationError::InvalidProviderCode {
                value: "Global/BI".to_string()
            }]
        );
    }

    #[test]
    fn provider_codes_ortho4xp_actually_ships_are_accepted() {
        for code in ["BI", "GO2", "Arc", "Arc@", "EOX", "USA2", "EUR.comb"] {
            let mut errors = Vec::new();
            let mut p = parameters();
            p.provider = code.to_string();
            validate_parameters(&p, &mut errors);
            assert!(errors.is_empty(), "{code} rejected: {errors:?}");
        }
    }

    #[test]
    fn a_zoom_outside_the_band_is_a_fault() {
        for zoom in [9u8, 21u8] {
            let mut errors = Vec::new();
            let mut p = parameters();
            p.zoom = zoom;
            validate_parameters(&p, &mut errors);
            assert_eq!(errors, vec![ValidationError::ZoomOutOfRange { zoom }]);
        }
    }

    #[test]
    fn a_raw_key_owned_by_a_curated_field_is_a_fault() {
        let mut errors = Vec::new();
        let mut p = parameters();
        p.raw.insert("default_zl".to_string(), "18".to_string());
        p.raw
            .insert("default_website".to_string(), "GO2".to_string());
        validate_parameters(&p, &mut errors);
        assert_eq!(
            errors,
            vec![
                ValidationError::ReservedRawKey {
                    key: "default_website".to_string(),
                    curated_field: "provider",
                },
                ValidationError::ReservedRawKey {
                    key: "default_zl".to_string(),
                    curated_field: "zoom",
                },
            ]
        );
    }

    #[test]
    fn an_unreserved_raw_key_is_fine() {
        let mut errors = Vec::new();
        let mut p = parameters();
        p.raw.insert(
            "cover_airports_with_highres".to_string(),
            "ICAO".to_string(),
        );
        validate_parameters(&p, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
    }

    fn metadata() -> Metadata {
        Metadata {
            name: "North America".to_string(),
            region_code: "NA".to_string(),
            revision: 1,
        }
    }

    #[test]
    fn well_formed_metadata_passes() {
        let mut errors = Vec::new();
        validate_metadata(&metadata(), &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_blank_name_is_a_fault() {
        let mut errors = Vec::new();
        let mut m = metadata();
        m.name = "   ".to_string();
        validate_metadata(&m, &mut errors);
        assert_eq!(errors, vec![ValidationError::EmptyName]);
    }

    #[test]
    fn region_codes_in_use_are_accepted() {
        for code in ["NA", "OC", "EU-1", "AS-4"] {
            let mut errors = Vec::new();
            let mut m = metadata();
            m.region_code = code.to_string();
            validate_metadata(&m, &mut errors);
            assert!(errors.is_empty(), "{code} rejected: {errors:?}");
        }
    }

    #[test]
    fn a_lowercase_region_code_is_a_fault() {
        let mut errors = Vec::new();
        let mut m = metadata();
        m.region_code = "na".to_string();
        validate_metadata(&m, &mut errors);
        assert_eq!(
            errors,
            vec![ValidationError::InvalidRegionCode {
                value: "na".to_string()
            }]
        );
    }

    #[test]
    fn revision_zero_is_a_fault() {
        let mut errors = Vec::new();
        let mut m = metadata();
        m.revision = 0;
        validate_metadata(&m, &mut errors);
        assert_eq!(errors, vec![ValidationError::RevisionTooLow]);
    }

    #[test]
    fn an_absolute_target_root_passes_and_a_relative_one_does_not() {
        let mut errors = Vec::new();
        validate_target(
            &TargetLocation {
                root: std::path::PathBuf::from("/srv/oxo/artifacts/NA"),
            },
            &mut errors,
        );
        assert!(errors.is_empty(), "{errors:?}");

        let mut errors = Vec::new();
        validate_target(
            &TargetLocation {
                root: std::path::PathBuf::from("artifacts/NA"),
            },
            &mut errors,
        );
        assert_eq!(
            errors,
            vec![ValidationError::TargetRootNotAbsolute {
                value: "artifacts/NA".to_string()
            }]
        );
    }

    #[test]
    fn an_empty_target_root_is_a_fault() {
        let mut errors = Vec::new();
        validate_target(
            &TargetLocation {
                root: std::path::PathBuf::new(),
            },
            &mut errors,
        );
        assert_eq!(errors, vec![ValidationError::EmptyTargetRoot]);
    }

    #[test]
    fn zero_attempts_is_a_fault() {
        let mut errors = Vec::new();
        validate_failure_policy(
            &FailurePolicy {
                max_attempts: 0,
                backoff_seconds: 0,
                alert_destinations: Vec::new(),
            },
            &mut errors,
        );
        assert_eq!(errors, vec![ValidationError::MaxAttemptsTooLow]);
    }
}
