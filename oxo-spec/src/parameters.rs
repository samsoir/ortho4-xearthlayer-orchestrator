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

/// Longest accepted provider code.
///
/// Shared with the message that reports a rejection, so the bound and the
/// text that explains it cannot drift apart, and so the later API and web
/// interface enforce this bound rather than re-deriving one.
pub const PROVIDER_CODE_MAX_LEN: usize = 64;

/// Ortho4XP tile-configuration keys owned by curated fields, paired with
/// the field that owns each. A raw override naming one of these is a
/// validation error rather than a silent shadow.
///
/// `include_overlays` has no entry, because overlay generation is not an
/// Ortho4XP tile-configuration key at all: `Ortho4XP.py` never builds
/// overlays, and `O4_Tile_Utils.build_tile_list` gates them on a `do_ovl`
/// function argument. The flag decides whether the planner emits overlay
/// tasks for this region's tiles, so it is not Ortho4XP configuration and
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
    /// Whether the planner also emits an overlay task for each tile.
    /// `false` yields ortho tasks only, which is a supported choice rather
    /// than a degraded mode.
    #[serde(default)]
    pub include_overlays: bool,
    /// Ortho4XP tile-configuration keys passed through untouched, so that
    /// no tuning is unreachable.
    #[serde(default)]
    pub raw: BTreeMap<String, String>,
}

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
        let parameters: ProductionParameters = toml::from_str(text).expect("parse");
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
        let error =
            toml::from_str::<ProductionParameters>("provider = \"BI\"\nzoom = 16\nzoomm = 17\n")
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
