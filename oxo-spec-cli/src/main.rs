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
            // Serde stops at the first schema fault, so unlike validation
            // faults this is not the whole list. Say so, or the operator
            // reads it as the only problem.
            eprintln!("could not parse {}: {error}", path.display());
            eprintln!("fix this and re-run to see any remaining faults");
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
