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
    // `run` treats an *undefined* step as skipped, not failed, and still
    // exits 0 — so a typo or a step-regex rename would silently remove
    // acceptance coverage from the artifact that carries the acceptance
    // criteria. `fail_on_skipped` plus `run_and_exit` makes that a failure.
    SpecWorld::cucumber()
        .fail_on_skipped()
        .run_and_exit("features")
        .await;
}
