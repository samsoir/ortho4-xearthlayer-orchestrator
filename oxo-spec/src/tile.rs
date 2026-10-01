use std::fmt;

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
