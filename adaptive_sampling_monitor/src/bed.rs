//! BED4 validation with unique display names and nanalogue-compatible coordinates.

use std::collections::BTreeSet;
use std::io::BufRead;

use crate::error::{Context as _, Result, ensure, message};
use crate::text::is_printable_ascii;

/// Deliberate ceiling for the monitor's small target configuration.
const MAX_BED_BYTES: u64 = 100_000;

/// A named BED interval, retained in input order for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Region {
    /// Reference sequence name, matched exactly to BAM target names.
    pub contig: String,
    /// Inclusive zero-based reference coordinate.
    pub start: u32,
    /// Exclusive reference coordinate.
    pub end: u32,
    /// Unique fourth-column display label.
    pub name: String,
}

/// Rejects target files too large for this exploratory real-time display.
pub(crate) fn ensure_size(size: u64) -> Result<()> {
    ensure(
        size <= MAX_BED_BYTES,
        format!("BED exceeds the 100 kB size limit ({size} bytes)"),
    )
}

/// Parses BED4+, accepting blank, comment, track and browser lines.
///
/// Rejects empty intervals, negative coordinates, duplicate/empty labels and
/// control characters that could otherwise inject commands into the terminal.
pub(crate) fn parse_bed_regions<R: BufRead>(reader: R) -> Result<Vec<Region>> {
    let mut regions = Vec::new();
    let mut names = BTreeSet::new();
    for (offset, result) in reader.lines().enumerate() {
        let line_number = offset.saturating_add(1);
        let line = result.with_context(|| format!("reading BED line {line_number}"))?;
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with("track ")
            || trimmed.starts_with("browser ")
        {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let &[contig, start_text, end_text, name, ..] = fields.as_slice() else {
            return Err(message(format!(
                "BED line {line_number}: expected at least four tab-separated columns"
            )));
        };
        let start: i64 = start_text
            .parse()
            .with_context(|| format!("BED line {line_number}: invalid start"))?;
        let end: i64 = end_text
            .parse()
            .with_context(|| format!("BED line {line_number}: invalid end"))?;
        ensure(
            start >= 0 && end > start,
            format!("BED line {line_number}: require 0 <= start < end"),
        )?;
        ensure(
            !contig.is_empty() && contig == contig.trim() && is_printable_ascii(contig),
            format!(
                "BED line {line_number}: invalid contig; require nonempty printable ASCII without leading or trailing whitespace"
            ),
        )?;
        ensure(
            !name.trim().is_empty() && is_printable_ascii(name),
            format!("BED line {line_number}: invalid or empty name; require printable ASCII"),
        )?;
        ensure(
            name.len() <= 40,
            format!("BED line {line_number}: name exceeds 40 characters"),
        )?;
        ensure(
            names.insert(name.to_owned()),
            format!("BED line {line_number}: duplicate name '{name}'"),
        )?;
        regions.push(Region {
            contig: contig.to_owned(),
            start: u32::try_from(start)
                .with_context(|| format!("BED line {line_number}: start exceeds u32::MAX"))?,
            end: u32::try_from(end)
                .with_context(|| format!("BED line {line_number}: end exceeds u32::MAX"))?,
            name: name.to_owned(),
        });
    }
    ensure(!regions.is_empty(), "BED contains no regions")?;
    Ok(regions)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    //! Boundary and input validation tests.
    use super::*;

    /// Duplicate labels and malformed BED records fail before monitoring starts.
    #[test]
    fn rejects_invalid_input() {
        for (input, expected) in [
            ("chr1\t0\t20\n", "four tab-separated"),
            ("chr1\t0\t20\ta\nchr2\t3\t4\ta\n", "duplicate name 'a'"),
            ("chr1\tstart\t20\ta\n", "invalid start"),
            ("chr1\t0\tend\ta\n", "invalid end"),
            ("chr1\t-1\t20\ta\n", "0 <= start < end"),
            ("chr1\t20\t20\ta\n", "0 <= start < end"),
            (
                "chr1\t4294967296\t4294967297\ta\n",
                "start exceeds u32::MAX",
            ),
            ("chr1\t0\t4294967296\ta\n", "end exceeds u32::MAX"),
            ("chr1\t0\t20\t\n", "empty name"),
            ("chr1\t0\t20\t\u{1b}[31m\n", "invalid or empty name"),
            ("chr1\t0\t20\t\u{e9}\n", "printable ASCII"),
            ("chr1\t0\t20\ta\u{7f}\n", "printable ASCII"),
            ("chr\u{754c}\t0\t20\ta\n", "invalid contig"),
            ("# comment\n", "no regions"),
        ] {
            let error =
                parse_bed_regions(input.as_bytes()).expect_err("invalid BED must be rejected");
            assert!(
                error.to_string().contains(expected),
                "expected {expected}, got {error}"
            );
        }
    }

    /// Forty printable characters are accepted; the next character is rejected.
    #[test]
    fn label_length_boundary() -> Result<()> {
        let name = "A".repeat(40);
        let accepted = parse_bed_regions(format!("chr1\t0\t10\t{name}\n").as_bytes())?;
        assert_eq!(
            accepted.first().map(|region| &region.name),
            Some(&name),
            "40-character label retained"
        );
        let error = parse_bed_regions(format!("chr1\t0\t10\t{name}B\n").as_bytes())
            .expect_err("41-character label rejected");
        assert!(
            error.to_string().contains("exceeds 40"),
            "clear length error"
        );
        Ok(())
    }

    /// Metadata lines and columns after the label do not become regions.
    #[test]
    fn accepts_metadata_and_extra_columns() -> Result<()> {
        let regions = parse_bed_regions(
            &b"track name=targets\n browser position chr1\n# comment\n\nchr1\t0\t10\ta\t0\t+\n"[..],
        )?;
        assert_eq!(regions.len(), 1, "only the BED record is retained");
        Ok(())
    }

    /// Standard CRLF records work, but whitespace cannot silently alter a contig.
    #[test]
    fn crlf_and_contig_whitespace() -> Result<()> {
        let regions = parse_bed_regions(&b"chr1\t0\t10\ttarget\r\n"[..])?;
        assert_eq!(
            regions.first().map(|region| region.name.as_str()),
            Some("target"),
            "CRLF is removed before parsing the BED4 name"
        );
        for input in [" chr1\t0\t10\tleading\n", "chr1 \t0\t10\ttrailing\n"] {
            let error =
                parse_bed_regions(input.as_bytes()).expect_err("padded contig must be rejected");
            assert!(
                error.to_string().contains("leading or trailing whitespace"),
                "contig whitespace has an actionable diagnostic: {error}"
            );
        }
        Ok(())
    }

    /// The documented BED ceiling is inclusive and uses decimal kilobytes.
    #[test]
    fn file_size_boundary() -> Result<()> {
        ensure_size(MAX_BED_BYTES)?;
        let error = ensure_size(MAX_BED_BYTES.checked_add(1).context("size overflow")?)
            .expect_err("BED above 100 kB must be rejected");
        assert!(
            error.to_string().contains("100 kB size limit"),
            "size error is actionable: {error}"
        );
        Ok(())
    }
}
