//! Indexed BAM validation and per-BED-region nanalogue statistics.

use std::fs;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;
use std::str;

use nanalogue_core::{
    BamPreFilt as _, GenomicBed3, InputBamBuilder, nanalogue_indexed_bam_reader, read_stats,
};
use rust_htslib::bam::{FetchDefinition, Read as _};

use crate::bed::Region;
use crate::error::{Context as _, Result, ensure};
use crate::monitor::RegionReadStats;

/// Runs nanalogue read-stats on each indexed BED region with primary-only filtering.
pub(crate) fn scan_bam_regions(path: &Path, regions: &[Region]) -> Result<Vec<RegionReadStats>> {
    check_eof(path)?;
    let mut reader = nanalogue_indexed_bam_reader(path, FetchDefinition::All)?;
    reader.set_threads(2)?;
    ensure(
        reader.header().target_count() > 0,
        "BAM has no reference sequences; enable MinKNOW output alignment",
    )?;
    let mut stats = vec![RegionReadStats::default(); regions.len()];
    for (region, total) in regions.iter().zip(&mut stats) {
        let Some(tid) = reader.header().tid(region.contig.as_bytes()) else {
            continue;
        };
        let interval = GenomicBed3::new(i32::try_from(tid)?, region.start, region.end)?;
        reader.fetch((tid, region.start, region.end))?;
        let options = InputBamBuilder::default()
            .read_filter("primary_forward,primary_reverse".to_owned())
            .include_zero_len(true)
            .region_bed3(interval)
            .build()?;
        let mut report = Vec::new();
        let records = reader.rc_records().filter(|result| {
            result
                .as_ref()
                .map_or(true, |record| record.pre_filt(&options))
        });
        read_stats::run(&mut report, records)?;
        *total = parse_read_stats(&report)?;
    }
    Ok(stats)
}

/// Extracts the two read-stats fields needed for cross-BAM weighted means.
pub(crate) fn parse_read_stats(report: &[u8]) -> Result<RegionReadStats> {
    let text = str::from_utf8(report)?;
    let value = |key: &str| -> Result<u64> {
        let raw = text
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .find_map(|(name, value)| (name == key).then_some(value))
            .with_context(|| format!("nanalogue read-stats output missing '{key}'"))?;
        raw.parse()
            .with_context(|| format!("invalid '{key}' in nanalogue read-stats output"))
    };
    let count = value("n_primary_alignments")?;
    let mean = value("seq_len_mean")?;
    Ok(RegionReadStats {
        count,
        bases: count
            .checked_mul(mean)
            .context("read-stats length total overflow")?,
    })
}

/// Checks the standard BGZF end marker, not a checksum or integrity guarantee.
fn check_eof(path: &Path) -> Result<()> {
    // SAM/BAM specification section 4.1.2: the empty BGZF end-of-file block.
    const EOF: [u8; 28] = [
        0x1f, 0x8b, 0x08, 0x04, 0, 0, 0, 0, 0, 0xff, 0x06, 0, 0x42, 0x43, 0x02, 0, 0x1b, 0, 0x03,
        0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    let mut file = fs::File::open(path)?;
    let _position = file
        .seek(SeekFrom::End(-28))
        .context("BAM is incomplete: missing end marker")?;
    let mut trailer = [0; 28];
    file.read_exact(&mut trailer)?;
    ensure(trailer == EOF, "BAM is incomplete: missing end marker")?;
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    //! BAM footer validation failures before `HTSlib` opens the file.
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    /// Missing, truncated and wrong footer inputs all fail before BAM scanning.
    #[test]
    fn eof_validation_rejects_missing_short_and_invalid_files() -> Result<()> {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "nanalogue-adaptive-monitor-eof-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&directory)?;

        let missing = directory.join("missing.bam");
        let missing_error = check_eof(&missing).expect_err("missing BAM must fail");
        assert!(
            !missing_error.to_string().is_empty(),
            "filesystem error is retained"
        );

        let short = directory.join("short.bam");
        fs::write(&short, b"short")?;
        let short_error = check_eof(&short).expect_err("short BAM must fail");
        assert!(
            short_error.to_string().contains("missing end marker"),
            "short file has an actionable diagnostic"
        );

        let invalid = directory.join("invalid.bam");
        fs::write(&invalid, [0; 28])?;
        let invalid_error = check_eof(&invalid).expect_err("wrong footer must fail");
        assert!(
            invalid_error.to_string().contains("missing end marker"),
            "wrong footer has an actionable diagnostic"
        );

        fs::remove_dir_all(directory)?;
        Ok(())
    }

    /// Nanalogue output must be UTF-8 before field parsing begins.
    #[test]
    fn read_stats_rejects_invalid_utf8() {
        let error = parse_read_stats(&[0xff]).expect_err("non-UTF-8 report must fail");
        assert!(
            error.to_string().contains("invalid utf-8"),
            "UTF-8 diagnostic is retained: {error}"
        );
    }
}
