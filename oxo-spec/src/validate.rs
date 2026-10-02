use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::metadata::{Metadata, REGION_CODE_MAX_LEN};
use crate::parameters::{
    ProductionParameters, PROVIDER_CODE_MAX_LEN, RESERVED_RAW_KEYS, ZOOM_MAX, ZOOM_MIN,
};
use crate::policy::FailurePolicy;
use crate::target::TargetLocation;
use crate::tile::{TileId, TileIdParseError};

/// A single static-validation fault.
///
/// `#[non_exhaustive]`: three downstream sub-projects will match on this,
/// and a new rule must not break their builds.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
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
    MalformedRawKey {
        key: String,
        reason: &'static str,
    },
    MalformedRawValue {
        key: String,
        reason: &'static str,
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
                "provider code {value:?} is malformed: expected at most \
                 {PROVIDER_CODE_MAX_LEN} characters with no whitespace, \
                 control characters or path separators"
            ),
            Self::ReservedRawKey { key, curated_field } => write!(
                f,
                "raw override {key:?} is owned by the curated field \
                 {curated_field:?}; set {curated_field} instead of \
                 shadowing it"
            ),
            Self::MalformedRawKey { key, reason } => {
                write!(
                    f,
                    "raw override key {key:?} cannot be written out: {reason}"
                )
            }
            Self::MalformedRawValue { key, reason } => write!(
                f,
                "the value of raw override {key:?} cannot be written out: {reason}"
            ),
            Self::EmptyName => write!(f, "region name is empty"),
            Self::EmptyRegionCode => write!(f, "region code is empty"),
            Self::InvalidRegionCode { value } => write!(
                f,
                "region code {value:?} is malformed: expected at most \
                 {REGION_CODE_MAX_LEN} characters of A-Z, 0-9 and '-'"
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

    validate_raw_entries(&parameters.raw, errors);
}

/// Check that every raw override can survive being written into an
/// Ortho4XP tile configuration.
///
/// Ortho4XP reads that file as
/// `dict(line.strip().split("=") for line in f if line.strip())`
/// (`src/O4_Config_Utils.py:1413`, identically at 1444 and 1477), which
/// constrains both halves of every entry.
///
/// A **line break** in either half becomes a second configuration line, which
/// is how a value such as `"bar\ndefault_zl=18"` silently shadows
/// `default_zl` — exactly the fault class the reserved-key rule exists to
/// prevent.
///
/// A **`=`** in either half is worse. `dict()` over an iterable of sequences
/// requires each element to have exactly two items, and `"foo=a=b".split("=")`
/// yields three, so the call raises `ValueError` and the *entire* config read
/// fails. Per this project's recorded finding, Ortho4XP swallows that into a
/// bare `Crash!` with no traceback, so the operator learns neither that a raw
/// override was at fault nor which one. Nothing keeps the first split: that
/// would need `split("=", 1)` or `str.partition`, and the code uses neither.
///
/// This is a pure function of the model, so it is static validation and
/// belongs here. Every offending entry is reported, not the first.
fn validate_raw_entries(raw: &BTreeMap<String, String>, errors: &mut Vec<ValidationError>) {
    for (key, value) in raw {
        if let Some(reason) = raw_key_fault(key) {
            errors.push(ValidationError::MalformedRawKey {
                key: key.clone(),
                reason,
            });
        }
        if let Some(reason) = raw_value_fault(value) {
            errors.push(ValidationError::MalformedRawValue {
                key: key.clone(),
                reason,
            });
        }
    }
}

/// Why Ortho4XP could not be handed this raw key, or `None` if it can.
fn raw_key_fault(key: &str) -> Option<&'static str> {
    if key.is_empty() {
        Some("an override key cannot be empty")
    } else if key.contains('\n') || key.contains('\r') {
        Some(LINE_BREAK_REASON)
    } else if key.contains('=') {
        Some(EQUALS_REASON)
    } else if key.chars().any(char::is_control) {
        Some("a key cannot contain control characters")
    } else {
        None
    }
}

/// Why Ortho4XP could not be handed this raw value, or `None` if it can.
///
/// An empty value is legal — `"foo="` still splits into exactly two items —
/// and a control character other than a line break survives the read, so
/// neither is rejected here.
fn raw_value_fault(value: &str) -> Option<&'static str> {
    if value.contains('\n') || value.contains('\r') {
        Some(LINE_BREAK_REASON)
    } else if value.contains('=') {
        Some(EQUALS_REASON)
    } else {
        None
    }
}

/// Shared: a line break fails the same way on either side of the `=`.
const LINE_BREAK_REASON: &str =
    "Ortho4XP's tile configuration is one setting per line, so a line break \
     would inject a second setting";

/// Shared: a second `=` anywhere on the line fails the same way.
const EQUALS_REASON: &str =
    "Ortho4XP reads its tile configuration with dict(line.split(\"=\")), so a \
     second '=' on the line makes the whole config read raise, which it \
     reports only as Crash!";

fn is_well_formed_provider_code(code: &str) -> bool {
    code.len() <= PROVIDER_CODE_MAX_LEN
        && !code.contains('/')
        && !code.contains('\\')
        && code.chars().all(|c| !c.is_whitespace() && !c.is_control())
}

/// Validate region metadata.
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
    code.len() <= REGION_CODE_MAX_LEN
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
///
/// Target roots are **POSIX-absolute** — they begin with `/` — because OXO
/// targets Linux containers. The rule reads the string form rather than
/// asking `Path::is_absolute`, whose answer depends on the host OS: the same
/// specification text must validate identically on an operator's laptop and
/// in the Linux control plane, which is what "a pure function of the model"
/// means. A root that is not valid UTF-8 has no string form to check, so it
/// is reported as not absolute rather than panicking or passing silently.
pub(crate) fn validate_target(target: &TargetLocation, errors: &mut Vec<ValidationError>) {
    let Some(root) = target.root.to_str() else {
        errors.push(ValidationError::TargetRootNotAbsolute {
            value: target.root.to_string_lossy().into_owned(),
        });
        return;
    };

    if root.is_empty() {
        errors.push(ValidationError::EmptyTargetRoot);
    } else if !root.starts_with('/') {
        errors.push(ValidationError::TargetRootNotAbsolute {
            value: root.to_string(),
        });
    }
}

/// Validate the failure policy.
///
/// Only `max_attempts` needs a rule: `backoff_seconds` is unsigned, so the
/// design document's "non-negative backoff" requirement is enforced by the
/// type, and alert destinations are opaque until the observability
/// sub-project decides their representation.
pub(crate) fn validate_failure_policy(policy: &FailurePolicy, errors: &mut Vec<ValidationError>) {
    if policy.max_attempts < 1 {
        errors.push(ValidationError::MaxAttemptsTooLow);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant, paired with the fragment an operator needs to see.
    /// These strings are the crate's whole operator-facing product, so each
    /// is rendered rather than assumed.
    fn every_variant_with_its_fragment() -> Vec<(ValidationError, &'static str)> {
        vec![
            (ValidationError::EmptyTileSet, "tile set is empty"),
            (
                ValidationError::InvalidTileId {
                    value: "nope".to_string(),
                    reason: TileIdParseError::WrongLength { got: 4 },
                },
                "tile \"nope\" is not a valid identifier: expected 7 characters, got 4",
            ),
            (
                ValidationError::DuplicateTile {
                    value: "+50-002".to_string(),
                    occurrences: 2,
                },
                "tile +50-002 is listed 2 times",
            ),
            (
                ValidationError::ZoomOutOfRange { zoom: 99 },
                "zoom level 99 is outside 10..=20",
            ),
            (ValidationError::EmptyProviderCode, "provider code is empty"),
            (
                ValidationError::InvalidProviderCode {
                    value: "a b".to_string(),
                },
                "provider code \"a b\" is malformed: expected at most 64 characters",
            ),
            (
                ValidationError::ReservedRawKey {
                    key: "default_zl".to_string(),
                    curated_field: "zoom",
                },
                "raw override \"default_zl\" is owned by the curated field \"zoom\"",
            ),
            (
                ValidationError::MalformedRawKey {
                    key: String::new(),
                    reason: "an override key cannot be empty",
                },
                "raw override key \"\" cannot be written out: an override key cannot be empty",
            ),
            (
                ValidationError::MalformedRawValue {
                    key: "foo".to_string(),
                    reason: LINE_BREAK_REASON,
                },
                "the value of raw override \"foo\" cannot be written out: Ortho4XP's tile \
                 configuration is one setting per line",
            ),
            (ValidationError::EmptyName, "region name is empty"),
            (ValidationError::EmptyRegionCode, "region code is empty"),
            (
                ValidationError::InvalidRegionCode {
                    value: "na".to_string(),
                },
                "region code \"na\" is malformed: expected at most 16 characters",
            ),
            (
                ValidationError::RevisionTooLow,
                "revision must be at least 1",
            ),
            (ValidationError::EmptyTargetRoot, "target root is empty"),
            (
                ValidationError::TargetRootNotAbsolute {
                    value: "artifacts/NA".to_string(),
                },
                "target root \"artifacts/NA\" must be an absolute path",
            ),
            (
                ValidationError::MaxAttemptsTooLow,
                "failure policy max_attempts must be at least 1",
            ),
        ]
    }

    /// Exhaustive on purpose. A new `ValidationError` variant stops this
    /// compiling, which is the signal to give it a row in
    /// `every_variant_with_its_fragment` and bump the count asserted below.
    fn variant_name(error: &ValidationError) -> &'static str {
        match error {
            ValidationError::EmptyTileSet => "EmptyTileSet",
            ValidationError::InvalidTileId { .. } => "InvalidTileId",
            ValidationError::DuplicateTile { .. } => "DuplicateTile",
            ValidationError::ZoomOutOfRange { .. } => "ZoomOutOfRange",
            ValidationError::EmptyProviderCode => "EmptyProviderCode",
            ValidationError::InvalidProviderCode { .. } => "InvalidProviderCode",
            ValidationError::ReservedRawKey { .. } => "ReservedRawKey",
            ValidationError::MalformedRawKey { .. } => "MalformedRawKey",
            ValidationError::MalformedRawValue { .. } => "MalformedRawValue",
            ValidationError::EmptyName => "EmptyName",
            ValidationError::EmptyRegionCode => "EmptyRegionCode",
            ValidationError::InvalidRegionCode { .. } => "InvalidRegionCode",
            ValidationError::RevisionTooLow => "RevisionTooLow",
            ValidationError::EmptyTargetRoot => "EmptyTargetRoot",
            ValidationError::TargetRootNotAbsolute { .. } => "TargetRootNotAbsolute",
            ValidationError::MaxAttemptsTooLow => "MaxAttemptsTooLow",
        }
    }

    #[test]
    fn every_validation_error_renders_a_message_an_operator_can_act_on() {
        let table = every_variant_with_its_fragment();

        let mut names: Vec<&str> = table.iter().map(|(e, _)| variant_name(e)).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            16,
            "every ValidationError variant needs a row: {names:?}"
        );

        for (error, fragment) in &table {
            let rendered = error.to_string();
            assert!(
                rendered.contains(fragment),
                "{} rendered as {rendered:?}, which does not contain {fragment:?}",
                variant_name(error)
            );
        }
    }

    #[test]
    fn an_empty_report_renders_as_nothing_at_all() {
        assert_eq!(ValidationReport::new(Vec::new()).to_string(), "");
    }

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
    fn a_negative_zero_spelling_is_a_fault_not_a_silent_duplicate() {
        let mut errors = Vec::new();
        let input = vec!["+50+000".to_string(), "+50-000".to_string()];
        let tiles = validate_tiles(&input, &mut errors);
        assert_eq!(tiles.len(), 1);
        assert_eq!(
            errors,
            vec![ValidationError::InvalidTileId {
                value: "+50-000".to_string(),
                reason: TileIdParseError::NonCanonicalNegativeZero { field: "longitude" },
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

    #[test]
    fn an_empty_raw_key_is_a_fault() {
        let mut errors = Vec::new();
        let mut p = parameters();
        p.raw.insert(String::new(), "empty key".to_string());
        validate_parameters(&p, &mut errors);
        assert_eq!(
            errors,
            vec![ValidationError::MalformedRawKey {
                key: String::new(),
                reason: "an override key cannot be empty",
            }]
        );
    }

    #[test]
    fn a_raw_key_carrying_a_line_break_an_equals_or_a_control_character_is_a_fault() {
        for key in ["foo\nbar", "foo\rbar", "foo=bar", "foo\u{7}bar"] {
            let mut errors = Vec::new();
            let mut p = parameters();
            p.raw.insert(key.to_string(), "value".to_string());
            validate_parameters(&p, &mut errors);
            assert_eq!(
                errors.len(),
                1,
                "expected {key:?} to be rejected once: {errors:?}"
            );
            assert!(
                matches!(
                    &errors[0],
                    ValidationError::MalformedRawKey { key: reported, .. } if reported == key
                ),
                "expected {key:?} to be rejected: {errors:?}"
            );
        }
    }

    #[test]
    fn a_raw_value_carrying_a_line_break_is_a_fault() {
        for value in ["bar\ndefault_zl=18", "bar\rdefault_zl=18"] {
            let mut errors = Vec::new();
            let mut p = parameters();
            p.raw.insert("foo".to_string(), value.to_string());
            validate_parameters(&p, &mut errors);
            assert_eq!(
                errors,
                vec![ValidationError::MalformedRawValue {
                    key: "foo".to_string(),
                    reason: LINE_BREAK_REASON,
                }],
                "expected {value:?} to be rejected"
            );
        }
    }

    #[test]
    fn an_equals_sign_in_a_raw_value_is_a_fault_too() {
        // Not a first-split-wins read: Ortho4XP builds a dict from
        // `line.split("=")`, and `dict()` requires exactly two items per
        // element, so a second `=` raises ValueError and the whole config
        // read fails — reported to the operator only as `Crash!`.
        let mut errors = Vec::new();
        let mut p = parameters();
        p.raw.insert("custom_dem".to_string(), "a=b".to_string());
        validate_parameters(&p, &mut errors);
        assert_eq!(
            errors,
            vec![ValidationError::MalformedRawValue {
                key: "custom_dem".to_string(),
                reason: EQUALS_REASON,
            }]
        );
    }

    #[test]
    fn an_ordinary_raw_value_with_no_equals_or_line_break_passes() {
        let mut errors = Vec::new();
        let mut p = parameters();
        p.raw.insert(
            "cover_airports_with_highres".to_string(),
            "ICAO".to_string(),
        );
        // An empty value is legal: `"foo="` still splits into two items.
        p.raw.insert("custom_dem".to_string(), String::new());
        validate_parameters(&p, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn every_malformed_raw_entry_is_reported_not_just_the_first() {
        let mut errors = Vec::new();
        let mut p = parameters();
        p.raw.insert(String::new(), "fine".to_string());
        p.raw.insert("a=b".to_string(), "fine".to_string());
        p.raw.insert("good".to_string(), "two\nlines".to_string());
        validate_parameters(&p, &mut errors);
        assert_eq!(errors.len(), 3, "{errors:?}");
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
    fn a_windows_style_root_is_not_absolute_on_any_host() {
        let mut errors = Vec::new();
        validate_target(
            &TargetLocation {
                root: std::path::PathBuf::from("C:\\srv\\oxo"),
            },
            &mut errors,
        );
        assert_eq!(
            errors,
            vec![ValidationError::TargetRootNotAbsolute {
                value: "C:\\srv\\oxo".to_string()
            }]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_root_that_is_not_utf8_is_a_fault_rather_than_a_panic() {
        use std::os::unix::ffi::OsStringExt;

        let mut errors = Vec::new();
        validate_target(
            &TargetLocation {
                root: std::path::PathBuf::from(std::ffi::OsString::from_vec(vec![
                    b'/', 0xff, b'x',
                ])),
            },
            &mut errors,
        );
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            matches!(errors[0], ValidationError::TargetRootNotAbsolute { .. }),
            "{errors:?}"
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

    #[test]
    fn an_empty_region_code_is_a_fault() {
        let mut errors = Vec::new();
        let mut m = metadata();
        m.region_code = String::new();
        validate_metadata(&m, &mut errors);
        assert_eq!(errors, vec![ValidationError::EmptyRegionCode]);
    }

    #[test]
    fn the_region_code_length_ceiling_is_sixteen() {
        let mut errors = Vec::new();
        let mut m = metadata();
        m.region_code = "A".repeat(REGION_CODE_MAX_LEN);
        validate_metadata(&m, &mut errors);
        assert!(
            errors.is_empty(),
            "16 characters must be accepted: {errors:?}"
        );

        let mut errors = Vec::new();
        let mut m = metadata();
        m.region_code = "A".repeat(REGION_CODE_MAX_LEN + 1);
        validate_metadata(&m, &mut errors);
        assert_eq!(
            errors,
            vec![ValidationError::InvalidRegionCode {
                value: "A".repeat(REGION_CODE_MAX_LEN + 1)
            }]
        );
    }

    #[test]
    fn a_provider_code_with_a_backslash_is_a_fault() {
        let mut errors = Vec::new();
        let mut p = parameters();
        p.provider = "Global\\BI".to_string();
        validate_parameters(&p, &mut errors);
        assert_eq!(
            errors,
            vec![ValidationError::InvalidProviderCode {
                value: "Global\\BI".to_string()
            }]
        );
    }

    #[test]
    fn the_provider_code_length_ceiling_is_sixty_four() {
        let mut errors = Vec::new();
        let mut p = parameters();
        p.provider = "B".repeat(PROVIDER_CODE_MAX_LEN);
        validate_parameters(&p, &mut errors);
        assert!(
            errors.is_empty(),
            "64 characters must be accepted: {errors:?}"
        );

        let mut errors = Vec::new();
        let mut p = parameters();
        p.provider = "B".repeat(PROVIDER_CODE_MAX_LEN + 1);
        validate_parameters(&p, &mut errors);
        assert_eq!(
            errors,
            vec![ValidationError::InvalidProviderCode {
                value: "B".repeat(PROVIDER_CODE_MAX_LEN + 1)
            }]
        );
    }

    #[test]
    fn a_provider_code_with_whitespace_or_control_characters_is_a_fault() {
        for code in ["BI GO2", "BI\tGO2", "BI\u{7}"] {
            let mut errors = Vec::new();
            let mut p = parameters();
            p.provider = code.to_string();
            validate_parameters(&p, &mut errors);
            assert_eq!(
                errors,
                vec![ValidationError::InvalidProviderCode {
                    value: code.to_string()
                }],
                "expected {code:?} to be rejected"
            );
        }
    }
}
