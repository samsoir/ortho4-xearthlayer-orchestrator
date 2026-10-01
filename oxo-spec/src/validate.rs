use std::fmt;

use crate::parameters::{ZOOM_MAX, ZOOM_MIN};
use crate::tile::TileIdParseError;

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
}
