//! Small, real BAM fixtures exercise polling without sleeps or filesystem watchers.

use super::*;
use rust_htslib::bam::{self, Header, HeaderView, Record, header::HeaderRecord};
use std::fs::{File, FileTimes};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

/// Unique suffix for test directories created in the same process.
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

/// Test-owned directory removed recursively when its test finishes.
#[derive(Debug)]
struct TestDirectory(PathBuf);

impl TestDirectory {
    /// Creates a unique directory under the operating system's temporary root.
    fn new() -> Result<Self> {
        let suffix = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = std::env::temp_dir().join(format!(
            "nanalogue-adaptive-sampling-monitor-{}-{timestamp}-{suffix}",
            std::process::id()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    /// Returns the directory path for fixture construction.
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(&self.0);
    }
}

/// Two overlapping regions on chr1 and an independent region on chr2.
fn regions() -> Result<Vec<Region>> {
    crate::bed::parse(&b"chr1\t100\t200\tA\nchr1\t180\t300\tB\nchr2\t0\t100\tC\n"[..])
}

/// Writes SAM records into an unindexed BAM using deliberately reversed targets.
fn write_bam(path: &Path, records: &[&str]) -> Result<()> {
    let mut header = Header::new();
    let _chr2 = header.push_record(
        HeaderRecord::new(b"SQ")
            .push_tag(b"SN", "chr2")
            .push_tag(b"LN", 1000),
    );
    let _chr1 = header.push_record(
        HeaderRecord::new(b"SQ")
            .push_tag(b"SN", "chr1")
            .push_tag(b"LN", 1000),
    );
    let view = HeaderView::from_header(&header);
    let mut writer = bam::Writer::from_path(path, &header, bam::Format::Bam)?;
    for text in records {
        writer.write(&Record::from_sam(&view, text.as_bytes())?)?;
    }
    Ok(())
}

/// Executes a refresh immediately; production schedules the next one at 60s.
fn poll(monitor: &mut Monitor) {
    monitor.refresh();
}

/// New invalid BAM paths are reported and skipped without blocking valid files.
#[test]
fn skips_non_ascii_and_control_paths() -> Result<()> {
    let directory = TestDirectory::new()?;
    let good = "good\t0\tchr1\t101\t60\t3M\t*\t0\t0\tAAA\t*";
    write_bam(&directory.path().join("valid.bam"), &[good])?;
    let mut monitor = Monitor::new(directory.path().to_path_buf(), &regions()?);
    poll(&mut monitor);
    for name in ["\u{e9}.bam", "bad\n.bam", "\u{754c}/reads.bam"] {
        let path = directory.path().join(name);
        fs::create_dir_all(path.parent().context("fixture parent")?)?;
        write_bam(&path, &[good, good])?;
    }
    poll(&mut monitor);
    assert_eq!(
        monitor.snapshot.processed, 1,
        "invalid files are never accepted"
    );
    assert_eq!(
        monitor.snapshot.pending, 3,
        "each invalid BAM is reported as pending"
    );
    assert_eq!(
        monitor.snapshot.stats.first(),
        Some(&Stats { count: 1, bases: 3 }),
        "only ASCII path contributes"
    );
    let warning = monitor.snapshot.warning.as_ref().context("path warning")?;
    assert!(warning.contains("printable ASCII"), "actionable path error");
    assert!(
        crate::text::is_printable_ascii(warning),
        "safe path diagnostic"
    );
    Ok(())
}

/// Confirms half-open spans, reverse reads, CIGAR reference consumption and flags.
#[test]
fn primary_overlap_and_full_lengths() -> Result<()> {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("reads.bam");
    write_bam(
        &path,
        &[
            // chr1 [175,195), length 25 including five soft-clipped bases: A and B.
            "shared\t0\tchr1\t176\t60\t5S20M\t*\t0\t0\tAAAAAAAAAAAAAAAAAAAAAAAAA\t*",
            // Reverse primary, chr1 [195,205), length 10: A and B.
            "reverse\t16\tchr1\t196\t60\t10M\t*\t0\t0\tAAAAAAAAAA\t*",
            // [90,100) touches A but does not overlap it.
            "left\t0\tchr1\t91\t60\t10M\t*\t0\t0\tAAAAAAAAAA\t*",
            // [200,210) touches A's end and overlaps B only.
            "right\t0\tchr1\t201\t60\t10M\t*\t0\t0\tAAAAAAAAAA\t*",
            // Deletion spans [95,115), query length only 10: A.
            "deletion\t0\tchr1\t96\t60\t5M10D5M\t*\t0\t0\tAAAAAAAAAA\t*",
            // Insertion does not extend [90,100) into A despite query length 20.
            "insertion\t0\tchr1\t91\t60\t5M10I5M\t*\t0\t0\tAAAAAAAAAAAAAAAAAAAA\t*",
            // Hard-clipped bases are absent from SEQ: length seven, A only.
            "hard\t0\tchr1\t102\t60\t5H7M\t*\t0\t0\tAAAAAAA\t*",
            // Reference skip reaches B from [170,185), query length five.
            "skip\t0\tchr1\t171\t60\t2M10N3M\t*\t0\t0\tAAAAA\t*",
            // Excluded alignments may legitimately omit SEQ.
            "secondary\t256\tchr1\t181\t60\t10M\t*\t0\t0\t*\t*",
            "supplementary\t2048\tchr1\t181\t60\t10M\t*\t0\t0\t*\t*",
            "unmapped\t4\t*\t0\t0\t*\t*\t0\t0\tAAAAAAAAAA\t*",
            // No modification parsing: an invalid MM payload is irrelevant here.
            "other\t0\tchr2\t1\t0\t3M\t*\t0\t0\tAAA\t*\tMM:Z:nonsense",
        ],
    )?;
    let mut monitor = Monitor::new(directory.path().to_path_buf(), &regions()?);
    poll(&mut monitor);
    assert_eq!(
        monitor.snapshot.stats,
        vec![
            Stats {
                count: 5,
                bases: 57
            },
            Stats {
                count: 4,
                bases: 50
            },
            Stats { count: 1, bases: 3 }
        ],
        "full lengths are accumulated independently in each overlapping region"
    );
    assert_eq!(monitor.snapshot.processed, 1, "unindexed BAM is accepted");
    assert_eq!(
        monitor.snapshot.pending, 0,
        "no pending files after success"
    );
    Ok(())
}

/// Nanalogue intersections retain BED order, nested hits and absent-contig zeros.
#[test]
fn nested_half_open_overlaps() -> Result<()> {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("reads.bam");
    let targets = crate::bed::parse(
        &b"chr1\t100\t200\tright\nchr1\t0\t1000\touter\nchr1\t20\t40\tinner\nabsent\t0\t100\tmissing\nchr2\t0\t100\tother\n"[..],
    )?;
    write_bam(
        &path,
        &[
            // [40,100): touches both inner and right, intersects only outer.
            "touch\t0\tchr1\t41\t60\t1M58D1M\t*\t0\t0\tAA\t*",
            // [39,101): intersects inner, outer and right.
            "cross\t0\tchr1\t40\t60\t1M60D1M\t*\t0\t0\tAA\t*",
        ],
    )?;
    let actual = scan(&path, &targets)?;
    assert_eq!(
        actual,
        vec![
            Stats { count: 1, bases: 2 },
            Stats { count: 2, bases: 4 },
            Stats { count: 1, bases: 2 },
            Stats::default(),
            Stats::default(),
        ],
        "half-open intersection counts each target independently in BED order"
    );
    Ok(())
}

/// `CurrRead` validation must not be bypassed or allow a partial replacement.
#[test]
fn nanalogue_validation_retains_previous_contribution() -> Result<()> {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("reads.bam");
    let good = "good\t0\tchr1\t101\t60\t3M\t*\t0\t0\tAAA\t*";
    write_bam(&path, &[good])?;
    let mut monitor = Monitor::new(directory.path().to_path_buf(), &regions()?);
    poll(&mut monitor);
    let accepted = monitor.snapshot.stats.clone();
    assert_eq!(
        accepted.first(),
        Some(&Stats { count: 1, bases: 3 }),
        "valid initial data is accepted"
    );
    for (invalid, expected) in [
        (
            "paired\t1\tchr1\t101\t60\t3M\t*\t0\t0\tAAA\t*",
            "flags not supported",
        ),
        (
            "duplicate\t1024\tchr1\t101\t60\t3M\t*\t0\t0\tAAA\t*",
            "flags not supported",
        ),
        (
            "qc\t512\tchr1\t101\t60\t3M\t*\t0\t0\tAAA\t*",
            "flags not supported",
        ),
        (
            "missing\t0\tchr1\t101\t60\t3M\t*\t0\t0\t*\t*",
            "0-len sequences",
        ),
    ] {
        write_bam(&path, &[good, good, invalid])?;
        poll(&mut monitor);
        assert_eq!(
            monitor.snapshot.stats, accepted,
            "no partial data replaces the accepted file"
        );
        assert_eq!(monitor.snapshot.pending, 1, "invalid BAM remains pending");
        assert!(
            monitor
                .snapshot
                .warning
                .as_ref()
                .is_some_and(|warning| warning.contains(expected)),
            "nanalogue's validation error must be visible: {:?}",
            monitor.snapshot.warning
        );
    }
    write_bam(&path, &[good, good])?;
    poll(&mut monitor);
    assert_eq!(
        monitor.snapshot.stats.first(),
        Some(&Stats { count: 2, bases: 6 }),
        "valid retry replaces old data"
    );
    assert_eq!(
        monitor.snapshot.pending, 0,
        "valid retry clears pending state"
    );
    Ok(())
}

/// Both pass and fail are included; temporary files and unchanged data are not.
#[test]
fn recursive_discovery_replacement_and_retry() -> Result<()> {
    let directory = TestDirectory::new()?;
    for child in [
        "bam_pass/barcode01",
        "bam_fail",
        "tmp",
        "queued_reads",
        "nested.partial",
    ] {
        fs::create_dir_all(directory.path().join(child))?;
    }
    let pass = directory.path().join("bam_pass/barcode01/run_0.bam");
    let fail = directory.path().join("bam_fail/run_0.bam");
    let short = "short\t0\tchr1\t101\t60\t3M\t*\t0\t0\tAAA\t*";
    let long = "long\t0\tchr1\t101\t60\t7M\t*\t0\t0\tAAAAAAA\t*";
    write_bam(&pass, &[short])?;
    write_bam(&fail, &[long, long])?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(&pass, directory.path().join("linked.bam"))?;
    for name in [
        "tmp/open.bam",
        "queued_reads/open.bam",
        "nested.partial/open.bam",
        "working.tmp.bam",
        ".hidden.bam",
        "reads.bam.tmp",
    ] {
        fs::write(directory.path().join(name), b"not finished")?;
    }
    let mut monitor = Monitor::new(directory.path().to_path_buf(), &regions()?);
    poll(&mut monitor);
    assert_eq!(
        monitor.snapshot.stats.first(),
        Some(&Stats {
            count: 3,
            bases: 17
        }),
        "pass and fail contributions use a weighted sum"
    );
    assert_eq!(monitor.snapshot.processed, 2, "both final BAMs accepted");
    assert_eq!(monitor.snapshot.pending, 0, "temporary paths pruned");
    let updated = monitor.snapshot.updated;
    poll(&mut monitor);
    assert_eq!(
        monitor.snapshot.updated, updated,
        "unchanged files are not scanned twice"
    );

    write_bam(&pass, &[long, short, long])?;
    poll(&mut monitor);
    assert_eq!(
        monitor.snapshot.stats.first(),
        Some(&Stats {
            count: 5,
            bases: 31
        }),
        "changed file replaces, not adds to, the old contribution"
    );

    // A missing footer must not replace accepted data even if records can be read.
    let length = fs::metadata(&pass)?.len();
    File::options()
        .write(true)
        .open(&pass)?
        .set_len(length.checked_sub(28).context("fixture too small")?)?;
    poll(&mut monitor);
    assert_eq!(
        monitor.snapshot.stats.first(),
        Some(&Stats {
            count: 5,
            bases: 31
        }),
        "failed scan retains previous totals"
    );
    assert_eq!(
        monitor.snapshot.pending, 1,
        "incomplete update remains pending"
    );
    assert!(
        monitor
            .snapshot
            .warning
            .as_ref()
            .is_some_and(|text| text.contains("end marker")),
        "failure is visible"
    );

    write_bam(&pass, &[short])?;
    let next = directory.path().join("bam_pass/barcode01/run_1.bam");
    write_bam(&next, &[long])?;
    poll(&mut monitor);
    assert_eq!(
        monitor.snapshot.stats.first(),
        Some(&Stats {
            count: 4,
            bases: 24
        }),
        "retry can reduce old counts while new files add counts"
    );
    assert_eq!(monitor.snapshot.pending, 0, "repaired file is accepted");
    assert!(
        monitor.snapshot.warning.is_none(),
        "successful retry clears warning"
    );
    Ok(())
}

/// Metadata checks independently detect timestamp changes and size changes.
#[test]
fn fingerprint_and_change_during_scan() -> Result<()> {
    let directory = TestDirectory::new()?;
    let path = directory.path().join("reads.bam");
    let read = "read\t0\tchr1\t101\t60\t3M\t*\t0\t0\tAAA\t*";
    write_bam(&path, &[read])?;
    let initial = Fingerprint::read(&path)?;
    let later = initial
        .modified
        .checked_add(Duration::from_secs(2))
        .context("timestamp overflow")?;
    File::options()
        .write(true)
        .open(&path)?
        .set_times(FileTimes::new().set_modified(later))?;
    let touched = Fingerprint::read(&path)?;
    assert_eq!(initial.size, touched.size, "touch does not change size");
    assert_ne!(initial, touched, "timestamp alone detects a change");

    let mut monitor = Monitor::new(directory.path().to_path_buf(), &regions()?);
    poll(&mut monitor);
    let accepted = monitor.snapshot.stats.clone();
    let before = Fingerprint::read(&path)?;
    let scanned = scan(&path, &monitor.regions)?;
    write_bam(&path, &[read, read, read])?;
    File::options()
        .write(true)
        .open(&path)?
        .set_times(FileTimes::new().set_modified(before.modified))?;
    let after = Fingerprint::read(&path)?;
    assert_ne!(
        before.size, after.size,
        "fixture changes size independently of timestamp"
    );
    assert_eq!(
        before.modified, after.modified,
        "timestamp is deliberately preserved"
    );
    let error = monitor
        .accept(&path, before, scanned)
        .expect_err("changed file cannot commit");
    assert!(
        error.to_string().contains("changed while scanning"),
        "unstable scan is deferred"
    );
    assert_eq!(
        monitor.snapshot.stats, accepted,
        "failed commit leaves all totals intact"
    );
    poll(&mut monitor);
    assert_eq!(
        monitor.snapshot.stats.first(),
        Some(&Stats { count: 3, bases: 9 }),
        "next stable scan replaces the old contribution"
    );
    Ok(())
}
