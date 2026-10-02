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
