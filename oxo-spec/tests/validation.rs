use oxo_spec::{RegionSpec, SpecError, ValidationError};

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
fn a_valid_specification_is_accepted() {
    let spec = RegionSpec::from_toml(VALID).expect("should be valid");
    assert_eq!(spec.tiles.len(), 2);
    assert_eq!(spec.metadata.region_code, "NA");
    assert!(spec.parameters.include_overlays);
}

#[test]
fn every_fault_is_reported_not_just_the_first() {
    let text = r#"
tiles = ["+50-002", "+50-002", "nope"]

[metadata]
name = ""
region_code = "na"
revision = 0

[parameters]
provider = ""
zoom = 99

[parameters.raw]
default_zl = "18"

[target]
root = "artifacts/NA"

[failure_policy]
max_attempts = 0
"#;

    let error = RegionSpec::from_toml(text).expect_err("should be rejected");
    let SpecError::Validation(report) = error else {
        panic!("expected a validation failure, got {error}");
    };

    let expected = [
        ValidationError::DuplicateTile {
            value: "+50-002".to_string(),
            occurrences: 2,
        },
        ValidationError::ZoomOutOfRange { zoom: 99 },
        ValidationError::EmptyProviderCode,
        ValidationError::ReservedRawKey {
            key: "default_zl".to_string(),
            curated_field: "zoom",
        },
        ValidationError::EmptyName,
        ValidationError::InvalidRegionCode {
            value: "na".to_string(),
        },
        ValidationError::RevisionTooLow,
        ValidationError::TargetRootNotAbsolute {
            value: "artifacts/NA".to_string(),
        },
        ValidationError::MaxAttemptsTooLow,
    ];

    for fault in &expected {
        assert!(
            report.errors().contains(fault),
            "missing {fault:?} from:\n{report}"
        );
    }
    assert!(
        report
            .errors()
            .iter()
            .any(|e| matches!(e, ValidationError::InvalidTileId { .. })),
        "missing InvalidTileId from:\n{report}"
    );
}

#[test]
fn malformed_toml_is_a_parse_failure_not_a_validation_failure() {
    let error = RegionSpec::from_toml("this is not toml").expect_err("reject");
    assert!(matches!(error, SpecError::Parse(_)), "got {error}");
}

#[test]
fn a_validated_specification_round_trips_through_toml() {
    let spec = RegionSpec::from_toml(VALID).expect("valid");
    let rendered = toml::to_string_pretty(&spec).expect("serialise");
    let again =
        RegionSpec::from_toml(&rendered).expect("a rendered specification must still be valid");
    assert_eq!(spec, again);
}
