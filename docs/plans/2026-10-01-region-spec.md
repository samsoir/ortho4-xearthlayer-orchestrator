# Region Specification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A pure Rust library and thin CLI that model a region specification and statically validate it, reporting every fault at once.

**Architecture:** Two-stage parse-then-validate. `RawRegionSpec` deserializes permissively from TOML (tiles as strings) so that every fault can be collected; `RegionSpec::validate` converts it into a validated `RegionSpec` whose `TileId` values are guaranteed in range, or returns a `ValidationReport` carrying all faults. Validation rules are pure functions of the model — no filesystem, no network, no clock — which is what keeps anything requiring an Ortho4XP installation out of this crate.

**Tech Stack:** Rust (edition 2021), `serde` + `toml` for the model, `clap` for the CLI, `cucumber` for Gherkin acceptance features, GNU Make as the task front end.

**Spec:** `docs/specs/2026-10-01-region-spec-design.md` (and the architecture it inherits from, `docs/specs/2026-10-01-oxo-architecture-design.md`)

## Global Constraints

Every task's requirements implicitly include this section.

- **TDD is mandatory.** Write the failing test, run it and see it fail, then write the minimal code to pass. No implementation before a failing test.
- **SOLID.** Traits for abstraction, dependency injection, every unit testable in isolation.
- **Gherkin for acceptance criteria**, run by a cucumber-style framework.
- **Rust for all code in this sub-project.** No exceptions here; nothing in it touches Ortho4XP at runtime.
- **Test coverage: 80% minimum, 90%+ target.**
- **`make pre-commit` before every push.** Docs-only changes are exempt.
- **The library performs no I/O** beyond being handed specification text. No filesystem access, no network, no clock. This is a design commitment, not a convenience.
- **Validation reports every fault, never just the first.**
- **TOML is the canonical serialization format.** JSON falls out of the same serde types later; it is not a second format to maintain here.
- **Canonical tile identifier** is Ortho4XP's `short_latlon`: signed two-digit latitude, signed three-digit longitude, zero-padded after the sign — `+50-002`. Valid range is latitude -90..=89, longitude -180..=179.
- **Reserved raw keys** are exactly `default_website` (owned by curated field `provider`) and `default_zl` (owned by `zoom`). Verified in `Ortho4XP/src/O4_Cfg_Vars.py:268-269`.
- **Static zoom band is 10..=20**, a sanity bound only. Ortho4XP imposes no static bound; the authoritative ceiling is a provider's `max_zl` (e.g. `Providers/Global/EOX.lay` declares `max_zl=14`) and checking it is environmental validation owned by the control plane, not this crate.

### Reference fixture

Several tasks use this valid specification. TOML requires top-level keys before any table, so `tiles` must appear first — putting it after `[metadata]` would make it a metadata field and the parse would fail.

```toml
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
```

---

### Task 1: Workspace, toolchain and Make front end

**Files:**
- Create: `Cargo.toml`
- Create: `rust-toolchain.toml`
- Create: `.gitignore`
- Create: `Makefile`
- Create: `oxo-spec/Cargo.toml`
- Create: `oxo-spec/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: a workspace with one member crate `oxo-spec`; Make targets `build`, `check`, `test`, `test-strict`, `format`, `format-check`, `lint`, `coverage`, `verify`, `pre-commit`, `clean`, `help`. Later tasks run `make test` and `make verify`.

- [ ] **Step 1: Create the workspace manifest**

`Cargo.toml`:

```toml
[workspace]
members = ["oxo-spec"]
resolver = "2"

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "MIT"
rust-version = "1.74"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
toml = "0.8"
```

- [ ] **Step 2: Pin the toolchain**

`rust-toolchain.toml`:

```toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
```

- [ ] **Step 3: Ignore build output**

`.gitignore`:

```gitignore
/target
```

- [ ] **Step 4: Create the Make front end**

`Makefile`. **Recipe lines must be indented with a TAB, not spaces** — Make rejects spaces.

```makefile
.DEFAULT_GOAL := help
CARGO ?= cargo

.PHONY: help
help: ## Show this help message
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'

.PHONY: build
build: ## Build all crates (debug)
	$(CARGO) build --workspace

.PHONY: check
check: ## Fast compile check, no codegen
	$(CARGO) check --workspace --all-targets

.PHONY: test
test: ## Run all tests
	$(CARGO) test --workspace --all-targets

.PHONY: test-strict
test-strict: ## Run all tests with warnings as errors (matches CI)
	RUSTFLAGS="-D warnings" $(CARGO) test --workspace --all-targets

.PHONY: format
format: ## Format code
	$(CARGO) fmt --all

.PHONY: format-check
format-check: ## Check formatting without modifying
	$(CARGO) fmt --all -- --check

.PHONY: lint
lint: ## Run clippy
	$(CARGO) clippy --workspace --all-targets -- -D warnings

.PHONY: coverage
coverage: ## Coverage summary (requires cargo-llvm-cov)
	$(CARGO) llvm-cov --workspace --summary-only

.PHONY: verify
verify: format-check lint test-strict ## Format check, lint, test

.PHONY: pre-commit
pre-commit: verify ## REQUIRED before pushing

.PHONY: clean
clean: ## Remove build artifacts
	$(CARGO) clean
```

- [ ] **Step 5: Create the library crate manifest**

`oxo-spec/Cargo.toml`:

```toml
[package]
name = "oxo-spec"
description = "Region specification model and validation for the Ortho4 XEarthLayer Orchestrator"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
serde = { workspace = true }
toml = { workspace = true }
```

- [ ] **Step 6: Create the library root**

`oxo-spec/src/lib.rs`:

```rust
//! Region specification model and validation for the Ortho4 XEarthLayer
//! Orchestrator.
//!
//! This crate performs no I/O beyond being handed specification text. Any
//! validation that requires looking at an Ortho4XP installation, a
//! filesystem or a network is environmental validation and belongs to the
//! control plane, not here.

#![forbid(unsafe_code)]
```

- [ ] **Step 7: Verify the toolchain works end to end**

Run: `make verify`
Expected: PASS. Format check, clippy and the (empty) test run all succeed. If `cargo fmt` reports changes, run `make format` and re-run.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml rust-toolchain.toml .gitignore Makefile oxo-spec/
git commit -m "build: add Cargo workspace, pinned toolchain and Make front end"
```

---

### Task 2: TileId

**Files:**
- Create: `oxo-spec/src/tile.rs`
- Modify: `oxo-spec/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `TileId` (opaque, `Copy`, `Ord`, `Hash`) with associated constants `LAT_MIN: i8 = -90`, `LAT_MAX: i8 = 89`, `LON_MIN: i16 = -180`, `LON_MAX: i16 = 179`; `TileId::new(lat: i8, lon: i16) -> Result<TileId, TileIdParseError>`; `lat() -> i8`; `lon() -> i16`; `tile_dir_name() -> String`; `impl Display` producing the canonical form; `impl FromStr<Err = TileIdParseError>`; `Serialize`/`Deserialize` as the canonical string. `TileIdParseError` with variants `WrongLength { got: usize }`, `MissingLatSign`, `MissingLonSign`, `NonDigit { position: usize }`, `LatOutOfRange { lat: i32 }`, `LonOutOfRange { lon: i32 }`.

- [ ] **Step 1: Write the failing tests for canonical form**

Create `oxo-spec/src/tile.rs` containing only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_the_examples_from_the_design_document() {
        assert_eq!(TileId::new(50, -2).unwrap().to_string(), "+50-002");
        assert_eq!(TileId::new(-7, 110).unwrap().to_string(), "-07+110");
        assert_eq!(TileId::new(-90, -180).unwrap().to_string(), "-90-180");
        assert_eq!(TileId::new(89, 179).unwrap().to_string(), "+89+179");
    }

    #[test]
    fn tile_dir_name_matches_ortho4xp() {
        assert_eq!(
            TileId::new(50, -2).unwrap().tile_dir_name(),
            "zOrtho4XP_+50-002"
        );
    }
}
```

Add to `oxo-spec/src/lib.rs`:

```rust
pub mod tile;

pub use tile::{TileId, TileIdParseError};
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --package oxo-spec tile`
Expected: FAIL to compile — `cannot find type TileId in this scope`.

- [ ] **Step 3: Implement TileId construction and formatting**

Prepend to `oxo-spec/src/tile.rs`, above the test module:

```rust
use std::fmt;
use std::str::FromStr;

/// A 1x1 degree tile, identified by the integer latitude and longitude of
/// its south-west corner.
///
/// The canonical string form is Ortho4XP's `short_latlon`
/// (`src/O4_File_Names.py`): a signed two-digit latitude followed by a
/// signed three-digit longitude, zero padded after the sign. Latitude
/// takes two digits because 90 is the largest magnitude; longitude takes
/// three because 180 is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TileId {
    lat: i8,
    lon: i16,
}

/// Why a string could not be read as a [`TileId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TileIdParseError {
    WrongLength { got: usize },
    MissingLatSign,
    MissingLonSign,
    NonDigit { position: usize },
    LatOutOfRange { lat: i32 },
    LonOutOfRange { lon: i32 },
}

impl fmt::Display for TileIdParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongLength { got } => {
                write!(f, "expected 7 characters, got {got}")
            }
            Self::MissingLatSign => {
                write!(f, "latitude must start with '+' or '-'")
            }
            Self::MissingLonSign => {
                write!(f, "longitude must start with '+' or '-'")
            }
            Self::NonDigit { position } => {
                write!(f, "non-digit character at position {position}")
            }
            Self::LatOutOfRange { lat } => write!(
                f,
                "latitude {lat} is outside {}..={}",
                TileId::LAT_MIN,
                TileId::LAT_MAX
            ),
            Self::LonOutOfRange { lon } => write!(
                f,
                "longitude {lon} is outside {}..={}",
                TileId::LON_MIN,
                TileId::LON_MAX
            ),
        }
    }
}

impl std::error::Error for TileIdParseError {}

impl TileId {
    pub const LAT_MIN: i8 = -90;
    pub const LAT_MAX: i8 = 89;
    pub const LON_MIN: i16 = -180;
    pub const LON_MAX: i16 = 179;

    /// Construct a tile, rejecting coordinates outside the globe's 1x1
    /// degree grid.
    pub fn new(lat: i8, lon: i16) -> Result<Self, TileIdParseError> {
        if !(Self::LAT_MIN..=Self::LAT_MAX).contains(&lat) {
            return Err(TileIdParseError::LatOutOfRange { lat: lat.into() });
        }
        if !(Self::LON_MIN..=Self::LON_MAX).contains(&lon) {
            return Err(TileIdParseError::LonOutOfRange { lon: lon.into() });
        }
        Ok(Self { lat, lon })
    }

    pub fn lat(&self) -> i8 {
        self.lat
    }

    pub fn lon(&self) -> i16 {
        self.lon
    }

    /// Ortho4XP's tile directory name, e.g. `zOrtho4XP_+50-002`.
    pub fn tile_dir_name(&self) -> String {
        format!("zOrtho4XP_{self}")
    }
}

impl fmt::Display for TileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:+03}{:+04}", self.lat, self.lon)
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --package oxo-spec tile`
Expected: PASS, 2 tests.

- [ ] **Step 5: Commit**

```bash
git add oxo-spec/src/tile.rs oxo-spec/src/lib.rs
git commit -m "feat(spec): add TileId with Ortho4XP canonical formatting"
```

- [ ] **Step 6: Write the failing tests for parsing**

Add to the test module in `oxo-spec/src/tile.rs`:

```rust
    #[test]
    fn canonical_form_round_trips_across_the_entire_valid_range() {
        for lat in TileId::LAT_MIN..=TileId::LAT_MAX {
            for lon in TileId::LON_MIN..=TileId::LON_MAX {
                let tile = TileId::new(lat, lon).expect("in range");
                let text = tile.to_string();
                assert_eq!(text.len(), 7, "{text} is not 7 characters");
                assert_eq!(text.parse::<TileId>().expect("round trip"), tile);
            }
        }
    }

    #[test]
    fn rejects_coordinates_outside_the_range() {
        assert_eq!(
            TileId::new(90, 0),
            Err(TileIdParseError::LatOutOfRange { lat: 90 })
        );
        assert_eq!(
            TileId::new(0, 180),
            Err(TileIdParseError::LonOutOfRange { lon: 180 })
        );
        assert_eq!(
            "+90+000".parse::<TileId>(),
            Err(TileIdParseError::LatOutOfRange { lat: 90 })
        );
    }

    #[test]
    fn rejects_malformed_strings() {
        assert_eq!(
            "+50-02".parse::<TileId>(),
            Err(TileIdParseError::WrongLength { got: 6 })
        );
        assert_eq!(
            "50-002x".parse::<TileId>(),
            Err(TileIdParseError::MissingLatSign)
        );
        assert_eq!(
            "+50x002".parse::<TileId>(),
            Err(TileIdParseError::MissingLonSign)
        );
        assert_eq!(
            "+5a-002".parse::<TileId>(),
            Err(TileIdParseError::NonDigit { position: 2 })
        );
    }
```

- [ ] **Step 7: Run the tests to verify they fail**

Run: `cargo test --package oxo-spec tile`
Expected: FAIL to compile — `FromStr` is not implemented for `TileId`.

- [ ] **Step 8: Implement parsing**

Append to `oxo-spec/src/tile.rs`, above the test module:

```rust
impl FromStr for TileId {
    type Err = TileIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = s.as_bytes();
        if bytes.len() != 7 {
            return Err(TileIdParseError::WrongLength {
                got: s.chars().count(),
            });
        }
        let lat_sign = match bytes[0] {
            b'+' => 1i32,
            b'-' => -1i32,
            _ => return Err(TileIdParseError::MissingLatSign),
        };
        let lon_sign = match bytes[3] {
            b'+' => 1i32,
            b'-' => -1i32,
            _ => return Err(TileIdParseError::MissingLonSign),
        };

        let lat = read_digits(&bytes[1..3], 1)? * lat_sign;
        let lon = read_digits(&bytes[4..7], 4)? * lon_sign;

        if !(i32::from(Self::LAT_MIN)..=i32::from(Self::LAT_MAX)).contains(&lat) {
            return Err(TileIdParseError::LatOutOfRange { lat });
        }
        if !(i32::from(Self::LON_MIN)..=i32::from(Self::LON_MAX)).contains(&lon) {
            return Err(TileIdParseError::LonOutOfRange { lon });
        }

        Ok(Self {
            lat: lat as i8,
            lon: lon as i16,
        })
    }
}

/// Read `bytes` as a run of ASCII digits, reporting the absolute position
/// of the first offender. `offset` is where `bytes` starts in the whole
/// identifier, so error positions are meaningful to a reader.
fn read_digits(bytes: &[u8], offset: usize) -> Result<i32, TileIdParseError> {
    let mut value = 0i32;
    for (index, &byte) in bytes.iter().enumerate() {
        if !byte.is_ascii_digit() {
            return Err(TileIdParseError::NonDigit {
                position: offset + index,
            });
        }
        value = value * 10 + i32::from(byte - b'0');
    }
    Ok(value)
}
```

- [ ] **Step 9: Run the tests to verify they pass**

Run: `cargo test --package oxo-spec tile`
Expected: PASS, 5 tests. The exhaustive round-trip covers all 180 x 360 = 64,800 tiles and takes well under a second.

- [ ] **Step 10: Commit**

```bash
git add oxo-spec/src/tile.rs
git commit -m "feat(spec): parse TileId from canonical form with positioned errors"
```

- [ ] **Step 11: Write the failing test for serde**

Add to the test module in `oxo-spec/src/tile.rs`:

```rust
    #[test]
    fn serialises_as_the_canonical_string() {
        let tile = TileId::new(50, -2).unwrap();
        let json = serde_json::to_string(&tile).expect("serialise");
        assert_eq!(json, "\"+50-002\"");
        let back: TileId = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, tile);
    }

    #[test]
    fn deserialising_a_bad_identifier_reports_why() {
        let error = serde_json::from_str::<TileId>("\"nope\"")
            .expect_err("should reject");
        assert!(
            error.to_string().contains("expected 7 characters"),
            "unhelpful error: {error}"
        );
    }
```

Add `serde_json` as a dev-dependency so the test can exercise serde without TOML's table rules getting in the way:

```bash
cargo add serde_json --dev --package oxo-spec
```

- [ ] **Step 12: Run the tests to verify they fail**

Run: `cargo test --package oxo-spec tile`
Expected: FAIL to compile — `TileId` does not implement `Serialize`.

- [ ] **Step 13: Implement serde as the canonical string**

Append to `oxo-spec/src/tile.rs`, above the test module:

```rust
impl serde::Serialize for TileId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for TileId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let text = <String as serde::Deserialize>::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}
```

- [ ] **Step 14: Run the full verification**

Run: `make verify`
Expected: PASS, 7 tests in `oxo-spec`.

- [ ] **Step 15: Commit**

```bash
git add oxo-spec/src/tile.rs oxo-spec/Cargo.toml Cargo.lock
git commit -m "feat(spec): serialise TileId as its canonical string form"
```

---

### Task 3: ProductionParameters

**Files:**
- Create: `oxo-spec/src/parameters.rs`
- Modify: `oxo-spec/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `ProductionParameters { provider: String, zoom: u8, include_overlays: bool, raw: BTreeMap<String, String> }`, all public, `Serialize`/`Deserialize`, with `include_overlays` and `raw` defaulting when absent. Constants `ZOOM_MIN: u8 = 10`, `ZOOM_MAX: u8 = 20`, and `RESERVED_RAW_KEYS: &[(&str, &str)]` pairing each reserved Ortho4XP key with the curated field that owns it.

- [ ] **Step 1: Write the failing test**

Create `oxo-spec/src/parameters.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialises_curated_fields_and_the_raw_block() {
        let text = r#"
provider = "BI"
zoom = 16
include_overlays = true

[raw]
cover_airports_with_highres = "ICAO"
"#;
        let parameters: ProductionParameters =
            toml::from_str(text).expect("parse");
        assert_eq!(parameters.provider, "BI");
        assert_eq!(parameters.zoom, 16);
        assert!(parameters.include_overlays);
        assert_eq!(
            parameters.raw.get("cover_airports_with_highres"),
            Some(&"ICAO".to_string())
        );
    }

    #[test]
    fn overlays_and_raw_default_when_absent() {
        let parameters: ProductionParameters =
            toml::from_str("provider = \"BI\"\nzoom = 16\n").expect("parse");
        assert!(!parameters.include_overlays);
        assert!(parameters.raw.is_empty());
    }

    #[test]
    fn rejects_unknown_fields() {
        let error = toml::from_str::<ProductionParameters>(
            "provider = \"BI\"\nzoom = 16\nzoomm = 17\n",
        )
        .expect_err("should reject the typo");
        assert!(error.to_string().contains("zoomm"), "{error}");
    }

    #[test]
    fn reserved_keys_name_the_curated_field_that_owns_them() {
        assert_eq!(
            RESERVED_RAW_KEYS,
            [("default_website", "provider"), ("default_zl", "zoom")].as_slice()
        );
    }
}
```

Add to `oxo-spec/src/lib.rs`:

```rust
pub mod parameters;

pub use parameters::{ProductionParameters, RESERVED_RAW_KEYS, ZOOM_MAX, ZOOM_MIN};
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --package oxo-spec parameters`
Expected: FAIL to compile — `ProductionParameters` not found.

- [ ] **Step 3: Implement the type**

Prepend to `oxo-spec/src/parameters.rs`, above the test module:

```rust
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Lowest zoom level this crate will accept.
pub const ZOOM_MIN: u8 = 10;

/// Highest zoom level this crate will accept.
///
/// A sanity bound only. Ortho4XP imposes no static bound on `default_zl`,
/// and the authoritative ceiling belongs to the chosen provider — for
/// instance `Providers/Global/EOX.lay` declares `max_zl=14`. Checking that
/// requires reading an Ortho4XP installation, so it is environmental
/// validation and belongs to the control plane.
pub const ZOOM_MAX: u8 = 20;

/// Ortho4XP tile-configuration keys owned by curated fields, paired with
/// the field that owns each. A raw override naming one of these is a
/// validation error rather than a silent shadow.
///
/// `include_overlays` has no entry, because overlay generation is not an
/// Ortho4XP tile-configuration key at all: `Ortho4XP.py` never builds
/// overlays, and `O4_Tile_Utils.build_tile_list` gates them on a `do_ovl`
/// function argument. The flag decides whether the planner emits overlay
/// jobs for this region's tiles, so it is not Ortho4XP configuration and
/// has nothing to collide with.
pub const RESERVED_RAW_KEYS: &[(&str, &str)] =
    &[("default_website", "provider"), ("default_zl", "zoom")];

/// Production parameters for a region. One set per region — parameters do
/// not vary per tile, and a region needing mixed zoom levels is expressed
/// as more than one specification.
///
/// Field order matters for TOML serialisation: scalar values must be
/// emitted before tables, so `raw` comes last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductionParameters {
    /// Ortho4XP provider code, e.g. `BI`. Mapped to `default_website`.
    pub provider: String,
    /// Imagery zoom level. Mapped to `default_zl`.
    pub zoom: u8,
    /// Whether the planner also emits an overlay job for each tile.
    /// `false` yields ortho jobs only, which is a supported choice rather
    /// than a degraded mode.
    #[serde(default)]
    pub include_overlays: bool,
    /// Ortho4XP tile-configuration keys passed through untouched, so that
    /// no tuning is unreachable.
    #[serde(default)]
    pub raw: BTreeMap<String, String>,
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --package oxo-spec parameters`
Expected: PASS, 4 tests.

- [ ] **Step 5: Commit**

```bash
git add oxo-spec/src/parameters.rs oxo-spec/src/lib.rs
git commit -m "feat(spec): add ProductionParameters with curated fields and raw pass-through"
```

---

### Task 4: Metadata, TargetLocation and FailurePolicy

**Files:**
- Create: `oxo-spec/src/metadata.rs`
- Create: `oxo-spec/src/target.rs`
- Create: `oxo-spec/src/policy.rs`
- Modify: `oxo-spec/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `Metadata { name: String, region_code: String, revision: u32 }`; `TargetLocation { root: PathBuf }`; `FailurePolicy { max_attempts: u32, backoff_seconds: u64, alert_destinations: Vec<String> }`. All fields public, all three `Serialize`/`Deserialize` with `deny_unknown_fields`; `backoff_seconds` and `alert_destinations` default when absent.

- [ ] **Step 1: Write the failing tests**

Create `oxo-spec/src/metadata.rs`:

```rust
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
```

Create `oxo-spec/src/target.rs`:

```rust
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
```

Create `oxo-spec/src/policy.rs`:

```rust
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
        let policy: FailurePolicy =
            toml::from_str("max_attempts = 1\n").expect("parse");
        assert_eq!(policy.backoff_seconds, 0);
        assert!(policy.alert_destinations.is_empty());
    }
}
```

Add to `oxo-spec/src/lib.rs`:

```rust
pub mod metadata;
pub mod policy;
pub mod target;

pub use metadata::Metadata;
pub use policy::FailurePolicy;
pub use target::TargetLocation;
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --package oxo-spec`
Expected: FAIL to compile — `Metadata`, `TargetLocation` and `FailurePolicy` not found.

- [ ] **Step 3: Implement the three types**

Prepend to `oxo-spec/src/metadata.rs`:

```rust
use serde::{Deserialize, Serialize};

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
```

Prepend to `oxo-spec/src/target.rs`:

```rust
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
```

Prepend to `oxo-spec/src/policy.rs`:

```rust
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --package oxo-spec`
Expected: PASS, 4 new tests (11 total).

- [ ] **Step 5: Commit**

```bash
git add oxo-spec/src/metadata.rs oxo-spec/src/target.rs oxo-spec/src/policy.rs oxo-spec/src/lib.rs
git commit -m "feat(spec): add metadata, target location and failure policy types"
```

---

### Task 5: RawRegionSpec and TOML parsing

**Files:**
- Create: `oxo-spec/src/raw.rs`
- Modify: `oxo-spec/src/lib.rs`

**Interfaces:**
- Consumes: `Metadata`, `ProductionParameters`, `TargetLocation`, `FailurePolicy`.
- Produces: `RawRegionSpec { tiles: Vec<String>, metadata: Metadata, parameters: ProductionParameters, target: TargetLocation, failure_policy: FailurePolicy }` and `RawRegionSpec::from_toml(text: &str) -> Result<RawRegionSpec, toml::de::Error>`. Tiles stay as strings here deliberately: validation must be able to report every malformed identifier, which is impossible if deserialization rejects the first one.

- [ ] **Step 1: Write the failing test**

Create `oxo-spec/src/raw.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

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
        let raw = RawRegionSpec::from_toml(&text)
            .expect("deserialisation must not reject a bad tile");
        assert_eq!(raw.tiles, vec!["+50-002", "nope"]);
    }

    #[test]
    fn rejects_an_unknown_section() {
        let text = format!("{VALID}\n[nonsense]\nkey = 1\n");
        let error = RawRegionSpec::from_toml(&text).expect_err("should reject");
        assert!(error.to_string().contains("nonsense"), "{error}");
    }
}
```

Add to `oxo-spec/src/lib.rs`:

```rust
pub mod raw;

pub use raw::RawRegionSpec;
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --package oxo-spec raw`
Expected: FAIL to compile — `RawRegionSpec` not found.

- [ ] **Step 3: Implement the raw specification**

Prepend to `oxo-spec/src/raw.rs`:

```rust
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --package oxo-spec raw`
Expected: PASS, 3 tests.

- [ ] **Step 5: Commit**

```bash
git add oxo-spec/src/raw.rs oxo-spec/src/lib.rs
git commit -m "feat(spec): deserialise RawRegionSpec from canonical TOML"
```

---

### Task 6: ValidationError and ValidationReport

**Files:**
- Create: `oxo-spec/src/validate.rs`
- Modify: `oxo-spec/src/lib.rs`

**Interfaces:**
- Consumes: `TileIdParseError`.
- Produces: `ValidationError` with variants `EmptyTileSet`, `InvalidTileId { value: String, reason: TileIdParseError }`, `DuplicateTile { value: String, occurrences: usize }`, `ZoomOutOfRange { zoom: u8 }`, `EmptyProviderCode`, `InvalidProviderCode { value: String }`, `ReservedRawKey { key: String, curated_field: &'static str }`, `EmptyName`, `EmptyRegionCode`, `InvalidRegionCode { value: String }`, `RevisionTooLow`, `EmptyTargetRoot`, `TargetRootNotAbsolute { value: String }`, `MaxAttemptsTooLow`; all with `Display`. `ValidationReport::new(Vec<ValidationError>)`, `errors() -> &[ValidationError]`, `len()`, `is_empty()`, `Display` (one fault per line), and `impl std::error::Error`.

- [ ] **Step 1: Write the failing test**

Create `oxo-spec/src/validate.rs`:

```rust
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
```

Add to `oxo-spec/src/lib.rs`:

```rust
pub mod validate;

pub use validate::{ValidationError, ValidationReport};
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --package oxo-spec validate`
Expected: FAIL to compile — `ValidationReport` not found.

- [ ] **Step 3: Implement the error types**

Prepend to `oxo-spec/src/validate.rs`:

```rust
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
            Self::ZoomOutOfRange { zoom } => write!(
                f,
                "zoom level {zoom} is outside {ZOOM_MIN}..={ZOOM_MAX}"
            ),
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --package oxo-spec validate`
Expected: PASS, 3 tests.

- [ ] **Step 5: Commit**

```bash
git add oxo-spec/src/validate.rs oxo-spec/src/lib.rs
git commit -m "feat(spec): add ValidationError and all-faults ValidationReport"
```

---

### Task 7: Tile validation rules

**Files:**
- Modify: `oxo-spec/src/validate.rs`

**Interfaces:**
- Consumes: `TileId`, `ValidationError`.
- Produces: `pub(crate) fn validate_tiles(raw: &[String], errors: &mut Vec<ValidationError>) -> BTreeSet<TileId>`. Pushes `EmptyTileSet`, `InvalidTileId` and `DuplicateTile`, and returns the tiles that did parse. Reports each distinct malformed value once, however many times it appears.

- [ ] **Step 1: Write the failing tests**

Add to the test module in `oxo-spec/src/validate.rs`:

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --package oxo-spec validate`
Expected: FAIL to compile — `validate_tiles` not found.

- [ ] **Step 3: Implement the rules**

Add these imports at the top of `oxo-spec/src/validate.rs`:

```rust
use std::collections::{BTreeMap, BTreeSet};

use crate::tile::TileId;
```

Append this function above the test module:

```rust
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --package oxo-spec validate`
Expected: PASS, 7 tests.

- [ ] **Step 5: Commit**

```bash
git add oxo-spec/src/validate.rs
git commit -m "feat(spec): validate tile sets for emptiness, duplicates and malformed ids"
```

---

### Task 8: Parameter validation rules

**Files:**
- Modify: `oxo-spec/src/validate.rs`

**Interfaces:**
- Consumes: `ProductionParameters`, `ZOOM_MIN`, `ZOOM_MAX`, `RESERVED_RAW_KEYS`, `ValidationError`.
- Produces: `pub(crate) fn validate_parameters(parameters: &ProductionParameters, errors: &mut Vec<ValidationError>)`. Pushes `EmptyProviderCode`, `InvalidProviderCode`, `ZoomOutOfRange` and `ReservedRawKey`.

- [ ] **Step 1: Write the failing tests**

Add to the test module in `oxo-spec/src/validate.rs`:

```rust
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
        p.raw
            .insert("cover_airports_with_highres".to_string(), "ICAO".to_string());
        validate_parameters(&p, &mut errors);
        assert!(errors.is_empty(), "{errors:?}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --package oxo-spec validate`
Expected: FAIL to compile — `validate_parameters` not found.

- [ ] **Step 3: Implement the rules**

Add this import at the top of `oxo-spec/src/validate.rs`:

```rust
use crate::parameters::{ProductionParameters, RESERVED_RAW_KEYS};
```

Append above the test module:

```rust
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
}

fn is_well_formed_provider_code(code: &str) -> bool {
    code.len() <= 64
        && !code.contains('/')
        && !code.contains('\\')
        && code
            .chars()
            .all(|c| !c.is_whitespace() && !c.is_control())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --package oxo-spec validate`
Expected: PASS, 14 tests.

- [ ] **Step 5: Commit**

```bash
git add oxo-spec/src/validate.rs
git commit -m "feat(spec): validate parameters including reserved raw-key collisions"
```

---

### Task 9: Metadata, target and policy validation rules

**Files:**
- Modify: `oxo-spec/src/validate.rs`

**Interfaces:**
- Consumes: `Metadata`, `TargetLocation`, `FailurePolicy`, `ValidationError`.
- Produces: `pub(crate) fn validate_metadata(metadata: &Metadata, errors: &mut Vec<ValidationError>)`, `pub(crate) fn validate_target(target: &TargetLocation, errors: &mut Vec<ValidationError>)`, `pub(crate) fn validate_failure_policy(policy: &FailurePolicy, errors: &mut Vec<ValidationError>)`.

- [ ] **Step 1: Write the failing tests**

Add to the test module in `oxo-spec/src/validate.rs`:

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --package oxo-spec validate`
Expected: FAIL to compile — `validate_metadata` not found.

- [ ] **Step 3: Implement the rules**

Add these imports at the top of `oxo-spec/src/validate.rs`:

```rust
use crate::metadata::Metadata;
use crate::policy::FailurePolicy;
use crate::target::TargetLocation;
```

Append above the test module:

```rust
/// Validate region metadata.
pub(crate) fn validate_metadata(
    metadata: &Metadata,
    errors: &mut Vec<ValidationError>,
) {
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
pub(crate) fn validate_target(
    target: &TargetLocation,
    errors: &mut Vec<ValidationError>,
) {
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
pub(crate) fn validate_failure_policy(
    policy: &FailurePolicy,
    errors: &mut Vec<ValidationError>,
) {
    if policy.max_attempts < 1 {
        errors.push(ValidationError::MaxAttemptsTooLow);
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --package oxo-spec validate`
Expected: PASS, 22 tests.

- [ ] **Step 5: Commit**

```bash
git add oxo-spec/src/validate.rs
git commit -m "feat(spec): validate metadata, target location and failure policy"
```

---

### Task 10: RegionSpec and the all-faults guarantee

**Files:**
- Create: `oxo-spec/src/spec.rs`
- Create: `oxo-spec/tests/validation.rs`
- Modify: `oxo-spec/src/lib.rs`

**Interfaces:**
- Consumes: every type and rule from Tasks 2-9.
- Produces: `RegionSpec { tiles: BTreeSet<TileId>, metadata: Metadata, parameters: ProductionParameters, target: TargetLocation, failure_policy: FailurePolicy }` (public fields, `Serialize`); `RegionSpec::validate(raw: RawRegionSpec) -> Result<RegionSpec, ValidationReport>`; `RegionSpec::from_toml(text: &str) -> Result<RegionSpec, SpecError>`; `SpecError::Parse(toml::de::Error)` and `SpecError::Validation(ValidationReport)` with `Display` and `std::error::Error`.

- [ ] **Step 1: Write the failing integration test**

Create `oxo-spec/tests/validation.rs`:

```rust
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
    let again = RegionSpec::from_toml(&rendered)
        .expect("a rendered specification must still be valid");
    assert_eq!(spec, again);
}
```

No dependency change is needed: Cargo makes a package's normal
`[dependencies]` available to test targets alongside its
`[dev-dependencies]`, so the integration test can use `toml` directly.

Add to `oxo-spec/src/lib.rs`:

```rust
pub mod spec;

pub use spec::{RegionSpec, SpecError};
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --package oxo-spec --test validation`
Expected: FAIL to compile — `RegionSpec` not found.

- [ ] **Step 3: Implement RegionSpec**

Create `oxo-spec/src/spec.rs`:

```rust
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
    validate_failure_policy, validate_metadata, validate_parameters,
    validate_target, validate_tiles, ValidationReport,
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
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --package oxo-spec --test validation`
Expected: PASS, 4 tests.

- [ ] **Step 5: Run the full verification**

Run: `make verify`
Expected: PASS. If clippy objects to `validate_tiles` and friends being `pub(crate)` while reachable from `spec.rs`, that is expected and correct — they are internal rules.

If clippy raises `result_large_err` on `SpecError` (it fires when the error variant exceeds 128 bytes, and `toml::de::Error` is not small), box the parse error: change the variant to `Parse(Box<toml::de::Error>)` and the constructor to `.map_err(|e| SpecError::Parse(Box::new(e)))`. Update the `Display` and `source` arms to match.

- [ ] **Step 6: Commit**

```bash
git add oxo-spec/src/spec.rs oxo-spec/src/lib.rs oxo-spec/tests/validation.rs oxo-spec/Cargo.toml Cargo.lock
git commit -m "feat(spec): add RegionSpec validation reporting every fault at once"
```

---

### Task 11: CLI

**Files:**
- Create: `oxo-spec-cli/Cargo.toml`
- Create: `oxo-spec-cli/src/main.rs`
- Create: `oxo-spec-cli/tests/cli.rs`
- Modify: `Cargo.toml`

**Interfaces:**
- Consumes: `RegionSpec::from_toml`, `SpecError`.
- Produces: binary `oxo-spec` with subcommands `validate <PATH>` (exit 0 and a one-line summary on success; exit 1 and every fault on stderr on failure) and `show <PATH>` (prints the normalised specification as TOML).

- [ ] **Step 1: Add the crate to the workspace**

Modify `Cargo.toml` to read:

```toml
[workspace]
members = ["oxo-spec", "oxo-spec-cli"]
resolver = "2"

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "MIT"
rust-version = "1.74"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
toml = "0.8"
clap = { version = "4", features = ["derive"] }
```

Create `oxo-spec-cli/Cargo.toml`:

```toml
[package]
name = "oxo-spec-cli"
description = "Command line interface for OXO region specifications"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[[bin]]
name = "oxo-spec"
path = "src/main.rs"

[dependencies]
clap = { workspace = true }
oxo-spec = { path = "../oxo-spec" }
toml = { workspace = true }
```

- [ ] **Step 2: Write the failing test**

Create `oxo-spec-cli/tests/cli.rs`:

```rust
use std::io::Write;
use std::process::Command;

const VALID: &str = r#"
tiles = ["+50-002"]

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

/// Write `text` to a uniquely named file under the target directory and
/// return its path. Using the build directory keeps the test from needing
/// a temp-file dependency.
fn fixture(name: &str, text: &str) -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("oxo-spec-cli-{name}-{}.toml", std::process::id()));
    let mut file = std::fs::File::create(&path).expect("create fixture");
    file.write_all(text.as_bytes()).expect("write fixture");
    path
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_oxo-spec"))
        .args(args)
        .output()
        .expect("run the binary")
}

#[test]
fn validate_accepts_a_valid_specification() {
    let path = fixture("valid", VALID);
    let output = run(&["validate", path.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains('1'), "expected a tile count: {stdout}");
    std::fs::remove_file(path).ok();
}

#[test]
fn validate_reports_every_fault_and_exits_non_zero() {
    let text = VALID
        .replace("zoom = 16", "zoom = 99")
        .replace("region_code = \"NA\"", "region_code = \"na\"");
    let path = fixture("invalid", &text);
    let output = run(&["validate", path.to_str().unwrap()]);
    assert!(!output.status.success(), "should have failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("zoom level 99"), "{stderr}");
    assert!(stderr.contains("region code"), "{stderr}");
    std::fs::remove_file(path).ok();
}

#[test]
fn show_prints_the_normalised_specification() {
    let path = fixture("show", VALID);
    let output = run(&["show", path.to_str().unwrap()]);
    assert!(output.status.success(), "should have succeeded");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("+50-002"), "{stdout}");
    assert!(stdout.contains("region_code"), "{stdout}");
    std::fs::remove_file(path).ok();
}

#[test]
fn a_missing_file_exits_non_zero_with_a_readable_message() {
    let output = run(&["validate", "/nonexistent/spec.toml"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("/nonexistent/spec.toml"), "{stderr}");
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test --package oxo-spec-cli`
Expected: FAIL to compile — `oxo-spec-cli/src/main.rs` does not exist.

- [ ] **Step 4: Implement the CLI**

Create `oxo-spec-cli/src/main.rs`:

```rust
//! Command line interface for OXO region specifications.
//!
//! Deliberately limited to validating and inspecting. Composing a tile set
//! belongs to the web interface; a half-measure here would become a second
//! authoring path to maintain and then deprecate.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use oxo_spec::{RegionSpec, SpecError};

#[derive(Debug, Parser)]
#[command(
    name = "oxo-spec",
    about = "Validate and inspect OXO region specifications"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Statically validate a specification, reporting every fault.
    Validate {
        /// Path to the specification TOML.
        path: PathBuf,
    },
    /// Print the parsed, normalised specification.
    Show {
        /// Path to the specification TOML.
        path: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let (path, show) = match cli.command {
        Command::Validate { path } => (path, false),
        Command::Show { path } => (path, true),
    };

    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("could not read {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    };

    let spec = match RegionSpec::from_toml(&text) {
        Ok(spec) => spec,
        Err(SpecError::Parse(error)) => {
            eprintln!("could not parse {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
        Err(SpecError::Validation(report)) => {
            eprintln!(
                "{} is not valid; {} fault(s):",
                path.display(),
                report.len()
            );
            eprintln!("{report}");
            return ExitCode::FAILURE;
        }
    };

    if show {
        match toml::to_string_pretty(&spec) {
            Ok(rendered) => print!("{rendered}"),
            Err(error) => {
                eprintln!("could not render specification: {error}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        println!(
            "{} is valid: region {}, {} tile(s)",
            path.display(),
            spec.metadata.region_code,
            spec.tiles.len()
        );
    }

    ExitCode::SUCCESS
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test --package oxo-spec-cli`
Expected: PASS, 4 tests.

- [ ] **Step 6: Run the full verification**

Run: `make verify`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock oxo-spec-cli/
git commit -m "feat(cli): add oxo-spec validate and show commands"
```

---

### Task 12: Gherkin acceptance features

**Files:**
- Create: `oxo-spec/features/region_spec.feature`
- Create: `oxo-spec/tests/acceptance.rs`
- Modify: `oxo-spec/Cargo.toml`

**Interfaces:**
- Consumes: `RegionSpec::from_toml`, `SpecError`, `ValidationError`.
- Produces: a cucumber test target named `acceptance` that runs `oxo-spec/features/*.feature`.

- [ ] **Step 1: Add the cucumber dev-dependency and test target**

Run: `cargo add cucumber --dev --package oxo-spec`
Accept whatever current version resolves; the derive macro and `World::run` entry point used below have been stable across recent releases. If the macro names differ, check `cargo doc --open --package cucumber`.

Then add to `oxo-spec/Cargo.toml`:

```toml
[[test]]
name = "acceptance"
harness = false
```

`harness = false` is required: cucumber provides its own runner.

- [ ] **Step 2: Write the failing feature**

Create `oxo-spec/features/region_spec.feature`:

```gherkin
Feature: Region specification validation
  As an operator preparing a regional scenery package
  I want every fault in my specification reported at once
  So that I can correct it in one pass rather than one run per mistake

  Background:
    Given a specification naming tiles "+50-002, +51-002"

  Scenario: A well-formed specification is accepted
    When I validate it
    Then it is accepted
    And it contains 2 tiles

  Scenario: An empty tile set is rejected
    Given a specification naming tiles ""
    When I validate it
    Then it is rejected
    And the report mentions "tile set is empty"

  Scenario: A duplicated tile is reported rather than silently collapsed
    Given a specification naming tiles "+50-002, +50-002"
    When I validate it
    Then it is rejected
    And the report mentions "is listed 2 times"

  Scenario: A malformed tile identifier is rejected
    Given a specification naming tiles "+50-002, nope"
    When I validate it
    Then it is rejected
    And the report mentions "is not a valid identifier"

  Scenario: A tile outside the valid range is rejected
    Given a specification naming tiles "+91+000"
    When I validate it
    Then it is rejected
    And the report mentions "latitude 91 is outside"

  Scenario: A raw override may not shadow a curated field
    Given the raw override "default_zl" is set to "18"
    When I validate it
    Then it is rejected
    And the report mentions "is owned by the curated field"

  Scenario: A zoom level outside the supported band is rejected
    Given the zoom level is 99
    When I validate it
    Then it is rejected
    And the report mentions "is outside 10..=20"

  Scenario: Independent faults are all reported together
    Given the zoom level is 99
    And the raw override "default_zl" is set to "18"
    And a specification naming tiles "+50-002, +50-002"
    When I validate it
    Then it is rejected
    And the report contains 3 faults
```

- [ ] **Step 3: Write the step definitions**

Create `oxo-spec/tests/acceptance.rs`:

```rust
use cucumber::{given, then, when, World};
use oxo_spec::{RegionSpec, SpecError};

/// Builds specification text from the pieces a scenario sets, so that each
/// step changes one thing and the TOML is assembled once at validation
/// time.
#[derive(Debug, World)]
#[world(init = Self::new)]
struct SpecWorld {
    tiles: Vec<String>,
    zoom: u8,
    raw: Vec<(String, String)>,
    outcome: Option<Result<RegionSpec, SpecError>>,
}

impl SpecWorld {
    fn new() -> Self {
        Self {
            tiles: vec!["+50-002".to_string()],
            zoom: 16,
            raw: Vec::new(),
            outcome: None,
        }
    }

    fn to_toml(&self) -> String {
        let tiles = self
            .tiles
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let raw = self
            .raw
            .iter()
            .map(|(key, value)| format!("{key} = \"{value}\"\n"))
            .collect::<String>();
        format!(
            "tiles = [{tiles}]\n\
             \n\
             [metadata]\n\
             name = \"North America\"\n\
             region_code = \"NA\"\n\
             revision = 1\n\
             \n\
             [parameters]\n\
             provider = \"BI\"\n\
             zoom = {zoom}\n\
             \n\
             [parameters.raw]\n\
             {raw}\n\
             [target]\n\
             root = \"/srv/oxo/artifacts/NA\"\n\
             \n\
             [failure_policy]\n\
             max_attempts = 3\n",
            tiles = tiles,
            zoom = self.zoom,
            raw = raw,
        )
    }

    fn report(&self) -> String {
        match self.outcome.as_ref().expect("validated") {
            Ok(_) => panic!("expected a rejection, but the spec was accepted"),
            Err(error) => error.to_string(),
        }
    }
}

#[given(regex = r#"^a specification naming tiles "(.*)"$"#)]
fn naming_tiles(world: &mut SpecWorld, tiles: String) {
    world.tiles = tiles
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(ToString::to_string)
        .collect();
}

#[given(regex = r"^the zoom level is (\d+)$")]
fn the_zoom_level_is(world: &mut SpecWorld, zoom: u8) {
    world.zoom = zoom;
}

#[given(regex = r#"^the raw override "(.+)" is set to "(.+)"$"#)]
fn the_raw_override(world: &mut SpecWorld, key: String, value: String) {
    world.raw.push((key, value));
}

#[when("I validate it")]
fn i_validate_it(world: &mut SpecWorld) {
    world.outcome = Some(RegionSpec::from_toml(&world.to_toml()));
}

#[then("it is accepted")]
fn it_is_accepted(world: &mut SpecWorld) {
    match world.outcome.as_ref().expect("validated") {
        Ok(_) => {}
        Err(error) => panic!("expected acceptance, got:\n{error}"),
    }
}

#[then("it is rejected")]
fn it_is_rejected(world: &mut SpecWorld) {
    assert!(
        world.outcome.as_ref().expect("validated").is_err(),
        "expected rejection, but the specification was accepted"
    );
}

#[then(regex = r"^it contains (\d+) tiles$")]
fn it_contains_tiles(world: &mut SpecWorld, expected: usize) {
    let spec = world
        .outcome
        .as_ref()
        .expect("validated")
        .as_ref()
        .expect("accepted");
    assert_eq!(spec.tiles.len(), expected);
}

#[then(regex = r#"^the report mentions "(.+)"$"#)]
fn the_report_mentions(world: &mut SpecWorld, fragment: String) {
    let report = world.report();
    assert!(
        report.contains(&fragment),
        "report did not mention {fragment:?}:\n{report}"
    );
}

#[then(regex = r"^the report contains (\d+) faults$")]
fn the_report_contains_faults(world: &mut SpecWorld, expected: usize) {
    let outcome = world.outcome.as_ref().expect("validated");
    let Err(SpecError::Validation(report)) = outcome else {
        panic!("expected a validation failure, got {outcome:?}");
    };
    assert_eq!(
        report.len(),
        expected,
        "expected {expected} faults, got {}:\n{report}",
        report.len()
    );
}

#[tokio::main]
async fn main() {
    SpecWorld::run("features").await;
}
```

Cucumber's runner is async, so add Tokio as a dev-dependency:

```bash
cargo add tokio --dev --package oxo-spec --features macros,rt-multi-thread
```

- [ ] **Step 4: Run the features to verify they fail**

Run: `cargo test --package oxo-spec --test acceptance`
Expected: FAIL — the feature file exists and the steps compile, but scenarios fail or the derive does not resolve until Step 3's code is complete. If every scenario already passes, the preceding tasks have delivered the behaviour and the feature is confirming it; re-read the output to be sure each scenario ran rather than being skipped as undefined.

- [ ] **Step 5: Make the features pass**

The behaviour is already implemented by Tasks 2-10. Fix only mismatches between the step text and the actual error strings. The expected fragments are:

- `tile set is empty` — from `ValidationError::EmptyTileSet`
- `is listed 2 times` — from `DuplicateTile`
- `is not a valid identifier` — from `InvalidTileId`
- `is owned by the curated field` — from `ReservedRawKey`
- `is outside 10..=20` — from `ZoomOutOfRange`
- `latitude 91 is outside` — from `TileIdParseError::LatOutOfRange`, wrapped in `InvalidTileId`

Run: `cargo test --package oxo-spec --test acceptance`
Expected: PASS, 8 scenarios.

- [ ] **Step 6: Run the full verification**

Run: `make verify`
Expected: PASS, all tests across both crates.

- [ ] **Step 7: Check coverage against the project floor**

Run: `make coverage`
Expected: 80% minimum, 90%+ target. If `cargo-llvm-cov` is not installed, install it with `cargo install cargo-llvm-cov` or record the gap and move on — coverage is not part of `verify`.

- [ ] **Step 8: Commit**

```bash
git add oxo-spec/features oxo-spec/tests/acceptance.rs oxo-spec/Cargo.toml Cargo.lock
git commit -m "test(spec): add Gherkin acceptance features for region specification validation"
```

---

## Completion

When all twelve tasks are done:

- `make verify` passes.
- `make coverage` reports at least 80%.
- Sub-project 1 is complete: the region specification can be authored by hand, validated with every fault reported at once, and inspected in normalised form.

Three of the design document's open decisions are deliberately untouched and remain open, with nothing in this implementation blocking them:

- **The exact curated parameter list** — `provider`, `zoom` and `include_overlays` are implemented; further curated fields are additive, and the raw pass-through covers them meanwhile.
- **Target location shape** — `TargetLocation` is a struct with one `root` field, so splitting ortho and overlay destinations later adds an optional field rather than breaking the format.
- **Revision semantics** — `revision: u32` is operator-set and validated as at least 1. Whether the job server keys jobs by it is sub-project 2's decision.
- **Alert destination representation** — `Vec<String>`, opaque, as the design document specifies until the observability sub-project decides.
