//! Printable-ASCII input validation and safe rendering of external messages.

use std::path::Path;

use crate::error::{Result, ensure};

/// Spaces and visible ASCII are each exactly one terminal cell.
pub(crate) fn is_printable_ascii(text: &str) -> bool {
    text.bytes().all(|byte| (b' '..=b'~').contains(&byte))
}

/// Preserves printable ASCII and visibly escapes all other characters.
pub(crate) fn escape(text: &str) -> String {
    let mut output = String::new();
    for character in text.chars() {
        if (' '..='~').contains(&character) {
            output.push(character);
        } else {
            output.extend(character.escape_default());
        }
    }
    output
}

/// Rejects non-ASCII and control bytes, including non-UTF-8 filenames.
pub(crate) fn check_path(path: &Path) -> Result<()> {
    ensure(
        path.to_str().is_some_and(is_printable_ascii),
        format!(
            "{}: path must contain only printable ASCII",
            path.as_os_str().as_encoded_bytes().escape_ascii()
        ),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Input and output policy boundary cases.
    use super::*;

    /// Escaping preserves ordinary punctuation but neutralizes terminal controls.
    #[test]
    fn external_text_is_ascii() {
        assert_eq!(
            escape("a '\\' \u{754c}\u{301}\n\t\u{1b}[31m\u{7f}"),
            "a '\\' \\u{754c}\\u{301}\\n\\t\\u{1b}[31m\\u{7f}",
            "wide, combining and control characters are escaped, not dropped"
        );
        assert!(
            is_printable_ascii(" !~"),
            "printable boundaries are allowed"
        );
        for invalid in ["\u{1f}", "\u{7f}", "\u{e9}", "\t"] {
            assert!(!is_printable_ascii(invalid), "non-printable input rejected");
            assert!(
                is_printable_ascii(&escape(invalid)),
                "escaped output is safe"
            );
        }
    }

    /// The policy checks full paths, not just their final filename.
    #[test]
    fn paths_require_printable_ascii() -> Result<()> {
        check_path(Path::new("run 1/bam_pass/reads.bam"))?;
        for invalid in ["run/\u{e9}.bam", "\u{754c}/reads.bam", "run/reads\n.bam"] {
            let error = check_path(Path::new(invalid)).expect_err("invalid path");
            assert!(is_printable_ascii(&error.to_string()), "safe error text");
        }
        Ok(())
    }

    /// Lossy conversion must not allow invalid filesystem bytes through.
    #[cfg(unix)]
    #[test]
    fn rejects_non_utf8_path() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;

        let error = check_path(Path::new(OsStr::from_bytes(b"run/\xff.bam")))
            .expect_err("non-UTF-8 path rejected");
        assert!(
            error.to_string().contains("\\xff"),
            "invalid byte is escaped"
        );
    }
}
