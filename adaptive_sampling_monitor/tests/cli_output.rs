//! Executable-level argument, validation and diagnostic coverage.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Creates an isolated directory using only standard-library facilities.
    fn test_directory() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "nanalogue-adaptive-monitor-cli-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("create test directory");
        directory
    }

    /// Runs the compiled monitor binary with the supplied arguments.
    fn run_monitor(arguments: &[&Path]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_nanalogue_adaptive_sampling_monitor"))
            .args(arguments)
            .output()
            .expect("run adaptive sampling monitor")
    }

    /// Help and version flags succeed without requiring input files or a terminal.
    #[test]
    fn help_and_version_output() {
        for (flag, expected) in [
            ("-h", "Usage: nanalogue_adaptive_sampling_monitor"),
            ("--help", "Usage: nanalogue_adaptive_sampling_monitor"),
            (
                "-V",
                concat!(
                    "nanalogue_adaptive_sampling_monitor ",
                    env!("CARGO_PKG_VERSION")
                ),
            ),
            (
                "--version",
                concat!(
                    "nanalogue_adaptive_sampling_monitor ",
                    env!("CARGO_PKG_VERSION")
                ),
            ),
        ] {
            let output = run_monitor(&[Path::new(flag)]);
            assert!(output.status.success(), "{flag} succeeds");
            assert!(
                String::from_utf8_lossy(&output.stdout).contains(expected),
                "{flag} prints expected output"
            );
            assert!(output.stderr.is_empty(), "{flag} has no diagnostic");
        }
    }

    /// Each pre-terminal validation stage returns a specific safe diagnostic.
    #[test]
    fn invalid_invocations_report_validation_stage() {
        let directory = test_directory();
        let valid_bed = directory.join("valid.bed");
        let invalid_bed = directory.join("invalid.bed");
        let oversized_bed = directory.join("oversized.bed");
        let missing_bed = directory.join("missing.bed");
        let missing_directory = directory.join("missing-directory");
        fs::write(&valid_bed, "chr1\t0\t10\ttarget\n").expect("write valid BED");
        fs::write(&invalid_bed, "not BED\n").expect("write invalid BED");
        fs::write(&oversized_bed, vec![b' '; 100_001]).expect("write oversized BED");

        let cases: Vec<(Vec<&Path>, &str)> = vec![
            (Vec::new(), "expected exactly two paths"),
            (
                vec![Path::new("--bogus"), directory.as_path()],
                "unknown option",
            ),
            (
                vec![Path::new("non-ascii-\u{e9}.bed"), directory.as_path()],
                "path must contain only printable ASCII",
            ),
            (vec![missing_bed.as_path(), directory.as_path()], "opening"),
            (
                vec![invalid_bed.as_path(), directory.as_path()],
                "expected at least four",
            ),
            (
                vec![oversized_bed.as_path(), directory.as_path()],
                "100 kB size limit",
            ),
            (
                vec![valid_bed.as_path(), missing_directory.as_path()],
                "not a directory",
            ),
            (
                vec![valid_bed.as_path(), Path::new("non-ascii-\u{e9}")],
                "path must contain only printable ASCII",
            ),
            (
                vec![valid_bed.as_path(), directory.as_path()],
                "requires an interactive terminal",
            ),
        ];
        for (arguments, expected) in cases {
            let output = run_monitor(&arguments);
            assert!(!output.status.success(), "invalid invocation fails");
            assert!(output.stdout.is_empty(), "invalid invocation has no stdout");
            let diagnostic = String::from_utf8_lossy(&output.stderr);
            assert!(
                diagnostic.starts_with("Error: ") && diagnostic.contains(expected),
                "expected {expected:?}, got {diagnostic:?}"
            );
        }

        fs::remove_dir_all(directory).expect("remove test directory");
    }
}
