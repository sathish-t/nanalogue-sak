//! Live terminal monitor of primary mapped reads overlapping named BED regions.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod bam_region_scan;
mod bed;
mod error;
mod monitor;
mod terminal_frame;
mod terminal_session;
mod text;

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufReader, IsTerminal as _, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

use error::{Context as _, Result, ensure};

/// Static help for the monitor's two positional arguments.
const HELP: &str = "Live per-BED-region read counts and mean lengths from MinKNOW BAM output

Usage: nanalogue_adaptive_sampling_monitor <BED_FILE> <DIRECTORY>

BED_FILE   BED4+ with unique printable-ASCII labels of at most 40 characters
DIRECTORY  Directory searched recursively for finalised BAMs every minute

Paths must contain only printable ASCII. Use -- before paths beginning with '-'.
-h, --help     Show this help
-V, --version  Show the version";

/// Command-line arguments for a single monitoring session.
#[derive(Debug)]
struct Args {
    /// BED4+ file with unique, nonempty names in column four.
    bed_file: PathBuf,
    /// Directory to search recursively for finalised BAMs every minute.
    directory: PathBuf,
}

impl Args {
    /// Requires exactly two paths; an initial `--` permits literal option-like names.
    fn parse(arguments: &[OsString]) -> Result<Self> {
        let mut paths = arguments.iter();
        if arguments.first().is_some_and(|arg| arg == "--") {
            let _separator = paths.next();
        } else {
            ensure(
                !arguments
                    .iter()
                    .any(|arg| arg.as_encoded_bytes().starts_with(b"-")),
                "unknown option; use --help, or -- before paths beginning with '-'",
            )?;
        }
        let usage = "expected exactly two paths: nanalogue_adaptive_sampling_monitor <BED_FILE> <DIRECTORY>";
        let bed_file = paths.next().context(usage)?;
        let directory = paths.next().context(usage)?;
        ensure(paths.next().is_none(), usage)?;
        Ok(Self {
            bed_file: bed_file.into(),
            directory: directory.into(),
        })
    }
}

/// Escapes external diagnostics even before the live screen has been entered.
fn main() -> ExitCode {
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    let flag = arguments.first().filter(|_| arguments.len() == 1);
    let result = if flag.is_some_and(|arg| arg == "-h" || arg == "--help") {
        writeln!(io::stdout(), "{HELP}").map_err(Into::into)
    } else if flag.is_some_and(|arg| arg == "-V" || arg == "--version") {
        writeln!(
            io::stdout(),
            "nanalogue_adaptive_sampling_monitor {}",
            env!("CARGO_PKG_VERSION")
        )
        .map_err(Into::into)
    } else {
        Args::parse(&arguments).and_then(run)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _written = writeln!(
                io::stderr(),
                "Error: {}",
                text::escape_terminal_text(&error.to_string())
            );
            ExitCode::FAILURE
        }
    }
}

/// Validates input before entering the alternate screen and starting any work.
fn run(args: Args) -> Result<()> {
    text::check_path(&args.bed_file)?;
    text::check_path(&args.directory)?;
    let input = File::open(&args.bed_file)
        .with_context(|| format!("opening {}", args.bed_file.display()))?;
    let bed_size = input
        .metadata()
        .with_context(|| format!("reading metadata for {}", args.bed_file.display()))?
        .len();
    bed::ensure_size(bed_size)?;
    let regions = bed::parse_bed_regions(BufReader::new(input))?;
    ensure(
        args.directory.is_dir(),
        format!("not a directory: {}", args.directory.display()),
    )?;
    ensure(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "this tool requires an interactive terminal",
    )?;

    // HTSlib writes diagnostics directly to stderr; scan errors are instead
    // reported in the status row, preserving the alternate-screen display.
    // SAFETY: This sets HTSlib's global logging level with a valid enum constant
    // before calling any other HTSlib functions.
    unsafe {
        rust_htslib::htslib::hts_set_log_level(rust_htslib::htslib::htsLogLevel_HTS_LOG_OFF);
    }
    let mut monitor = monitor::Monitor::new(args.directory, &regions);
    terminal_session::run_terminal_monitor(&regions, &mut monitor)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    //! Minimal positional parsing, including paths that resemble options.
    use super::*;

    /// Paths retain order and spaces; `--` makes following arguments literal.
    #[test]
    fn positional_paths() -> Result<()> {
        for (input, expected_bed, expected_directory) in [
            (
                vec!["targets with spaces.bed", "run 1"],
                "targets with spaces.bed",
                "run 1",
            ),
            (vec!["--", "--help", "-run"], "--help", "-run"),
        ] {
            let arguments = input.into_iter().map(OsString::from).collect::<Vec<_>>();
            let parsed = Args::parse(&arguments)?;
            assert_eq!(
                parsed.bed_file,
                PathBuf::from(expected_bed),
                "first path is BED"
            );
            assert_eq!(
                parsed.directory,
                PathBuf::from(expected_directory),
                "second path is directory"
            );
        }
        Ok(())
    }

    /// Missing/extra arguments and unknown options cannot silently become paths.
    #[test]
    fn rejects_invalid_arguments() {
        for input in [
            vec![],
            vec!["targets.bed"],
            vec!["targets.bed", "run", "extra"],
            vec!["--", "targets.bed"],
            vec!["--bogus", "run"],
            vec!["targets.bed", "--bogus"],
        ] {
            let arguments = input.into_iter().map(OsString::from).collect::<Vec<_>>();
            let error = Args::parse(&arguments).expect_err("invalid invocation rejected");
            assert!(
                text::is_printable_ascii(&error.to_string()),
                "diagnostic is ASCII"
            );
        }
    }
}
