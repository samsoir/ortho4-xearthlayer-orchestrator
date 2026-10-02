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
///
/// The mapping from string to `TileId` is injective: the form is
/// fixed-width with mandatory signs and zero-padded ASCII digits, so each
/// non-zero value has exactly one spelling, and zero has exactly one
/// because a `-` sign on a zero magnitude is rejected. One tile therefore
/// cannot be named by two different strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TileId {
    lat: i8,
    lon: i16,
}

/// Why a string could not be read as a [`TileId`].
///
/// `#[non_exhaustive]`: three downstream sub-projects will match on this,
/// and a new reason must not break their builds.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TileIdParseError {
    NonAscii,
    WrongLength {
        got: usize,
    },
    MissingLatSign,
    MissingLonSign,
    NonDigit {
        position: usize,
    },
    LatOutOfRange {
        lat: i32,
    },
    LonOutOfRange {
        lon: i32,
    },
    /// A `-` sign on a zero magnitude, e.g. `-00` or `-000`. Zero has one
    /// canonical spelling, so that a string denotes exactly one tile.
    NonCanonicalNegativeZero {
        field: &'static str,
    },
}

impl fmt::Display for TileIdParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonAscii => {
                write!(f, "identifier must be ASCII")
            }
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
            Self::NonCanonicalNegativeZero { field } => {
                write!(f, "{field} zero must be written with a + sign")
            }
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

impl FromStr for TileId {
    type Err = TileIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if !s.is_ascii() {
            return Err(TileIdParseError::NonAscii);
        }
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

        let lat_magnitude = read_digits(&bytes[1..3], 1)?;
        let lon_magnitude = read_digits(&bytes[4..7], 4)?;

        // Zero has one canonical spelling. Accepting `-00`/`-000` as well
        // would make two distinct strings denote one tile, which defeats
        // duplicate detection: the design document makes a repeated tile a
        // fault rather than a silent dedupe.
        if lat_sign < 0 && lat_magnitude == 0 {
            return Err(TileIdParseError::NonCanonicalNegativeZero { field: "latitude" });
        }
        if lon_sign < 0 && lon_magnitude == 0 {
            return Err(TileIdParseError::NonCanonicalNegativeZero { field: "longitude" });
        }

        let lat = lat_magnitude * lat_sign;
        let lon = lon_magnitude * lon_sign;

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

    #[test]
    fn lat_and_lon_report_the_coordinates_they_were_built_from() {
        let tile = TileId::new(-7, 110).expect("in range");
        assert_eq!(tile.lat(), -7);
        assert_eq!(tile.lon(), 110);
    }

    #[test]
    fn rejects_a_negative_zero_latitude() {
        assert_eq!(
            "-00+000".parse::<TileId>(),
            Err(TileIdParseError::NonCanonicalNegativeZero { field: "latitude" })
        );
    }

    #[test]
    fn rejects_a_negative_zero_longitude() {
        assert_eq!(
            "+50-000".parse::<TileId>(),
            Err(TileIdParseError::NonCanonicalNegativeZero { field: "longitude" })
        );
    }

    #[test]
    fn zero_has_exactly_one_accepted_spelling() {
        assert_eq!(
            "+00+000".parse::<TileId>().expect("canonical zero"),
            TileId::new(0, 0).expect("in range")
        );
        for spelling in ["-00+000", "+00-000", "-00-000"] {
            assert!(
                spelling.parse::<TileId>().is_err(),
                "{spelling} must not be accepted"
            );
        }
    }

    #[test]
    fn rejects_a_non_ascii_identifier_before_counting_length() {
        assert_eq!(
            "+50-00\u{a3}".parse::<TileId>(),
            Err(TileIdParseError::NonAscii)
        );
    }

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
        let error = serde_json::from_str::<TileId>("\"nope\"").expect_err("should reject");
        assert!(
            error.to_string().contains("expected 7 characters"),
            "unhelpful error: {error}"
        );
    }
}
