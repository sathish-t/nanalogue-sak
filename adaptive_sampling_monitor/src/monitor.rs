//! Recursive BAM discovery and transactional per-file statistics replacement.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::str;
use std::time::SystemTime;

use nanalogue_core::{
    BamPreFilt as _, GenomicBed3, InputBamBuilder, nanalogue_indexed_bam_reader, read_stats,
};
use rust_htslib::bam::{FetchDefinition, Read as _};

use crate::bed::Region;
use crate::error::{Context as _, Result, ensure};

/// Count and reconstructed length total from nanalogue read statistics.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stats {
    /// Number of primary mapped reads overlapping this region.
    pub count: u64,
    /// Primary count multiplied by nanalogue's integer mean sequence length.
    pub bases: u64,
}

impl Stats {
    /// Replaces one file's contribution in a global total.
    fn replace(self, old: Self, new: Self) -> Result<Self> {
        Ok(Self {
            count: self
                .count
                .checked_sub(old.count)
                .and_then(|value| value.checked_add(new.count))
                .context("read count overflow")?,
            bases: self
                .bases
                .checked_sub(old.bases)
                .and_then(|value| value.checked_add(new.bases))
                .context("base count overflow")?,
        })
    }
}

/// Cheap change detector, not a content-integrity guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    /// BAM size in bytes.
    bam_size: u64,
    /// BAM modification time at the filesystem's highest exposed resolution.
    bam_modified: SystemTime,
    /// BAI size in bytes.
    bai_size: u64,
    /// BAI modification time at the filesystem's highest exposed resolution.
    bai_modified: SystemTime,
}

impl Fingerprint {
    /// Reads BAM and MinKNOW-style `.bam.bai` metadata around each scan.
    fn read(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)?;
        let index_path = path.with_extension("bam.bai");
        let index_metadata = fs::metadata(&index_path)
            .with_context(|| format!("reading BAM index {}", index_path.display()))?;
        Ok(Self {
            bam_size: metadata.len(),
            bam_modified: metadata.modified()?,
            bai_size: index_metadata.len(),
            bai_modified: index_metadata.modified()?,
        })
    }
}

/// Only a successful, stable scan can create or replace a cached contribution.
#[derive(Debug)]
struct Contribution {
    /// Metadata corresponding to exactly these statistics.
    fingerprint: Fingerprint,
    /// Statistics in BED order.
    stats: Vec<Stats>,
}

/// Consistent statistics and status displayed between synchronous scans.
#[derive(Debug)]
pub(crate) struct Snapshot {
    /// Totals in BED-file order.
    pub stats: Vec<Stats>,
    /// Number of files whose contributions are included.
    pub processed: usize,
    /// New, changed or unreadable files still awaiting a successful scan.
    pub pending: usize,
    /// Human-readable activity, never raw terminal escape sequences.
    pub activity: String,
    /// Most recent problem in this poll, cleared when the next poll succeeds.
    pub warning: Option<String>,
}

impl Snapshot {
    /// Empty initial view, before directory enumeration begins.
    pub(crate) fn new(region_count: usize) -> Self {
        Self {
            stats: vec![Stats::default(); region_count],
            processed: 0,
            pending: 0,
            activity: "Discovering BAM files".to_owned(),
            warning: None,
        }
    }
}

/// Per-file cache and current display state.
#[derive(Debug)]
pub(crate) struct Monitor {
    /// Target directory; descendants are included without following symlinks.
    directory: PathBuf,
    /// Named targets in BED order, resolved against each BAM's own header.
    regions: Vec<Region>,
    /// Last accepted contributions, keyed by paths that must remain present.
    files: BTreeMap<PathBuf, Contribution>,
    /// Current global totals and processing status.
    snapshot: Snapshot,
}

impl Monitor {
    /// Constructs an empty monitor; the first refresh includes existing files.
    pub(crate) fn new(directory: PathBuf, regions: &[Region]) -> Self {
        Self {
            directory,
            regions: regions.to_vec(),
            files: BTreeMap::new(),
            snapshot: Snapshot::new(regions.len()),
        }
    }

    /// Discovers changes and updates the display state while retaining good old data.
    pub(crate) fn refresh(&mut self) -> Result<()> {
        self.ensure_processed_files_exist()?;
        self.snapshot.warning = None;
        self.snapshot.pending = 0;
        "Discovering BAM files".clone_into(&mut self.snapshot.activity);
        let mut candidates = Vec::new();
        let directory = self.directory.clone();
        self.discover(&directory, &mut candidates);
        candidates.sort_by(|left, right| left.0.cmp(&right.0));
        self.snapshot.pending = self.snapshot.pending.saturating_add(candidates.len());
        for (path, before) in candidates {
            let result =
                scan(&path, &self.regions).and_then(|stats| self.accept(&path, before, stats));
            match result {
                Ok(()) => {
                    self.snapshot.pending = self
                        .snapshot
                        .pending
                        .checked_sub(1)
                        .context("pending BAM count underflow")?;
                }
                Err(error) => {
                    self.snapshot.warning = Some(format!("{}: {error}", path.display()));
                }
            }
        }
        "Watching; checking for changes every 60s".clone_into(&mut self.snapshot.activity);
        Ok(())
    }

    /// Returns the latest complete statistics and monitoring status.
    pub(crate) fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// Stops rather than double-counting a processed BAM that moved to a new path.
    fn ensure_processed_files_exist(&self) -> Result<()> {
        for path in self.files.keys() {
            let exists = path
                .try_exists()
                .with_context(|| format!("checking processed BAM {}", path.display()))?;
            ensure(
                exists,
                format!(
                    "processed BAM disappeared; refusing to risk counting a moved file twice: {}",
                    path.display()
                ),
            )?;
        }
        Ok(())
    }

    /// Recurses through eligible directories without following symbolic links.
    fn discover(&mut self, directory: &Path, candidates: &mut Vec<(PathBuf, Fingerprint)>) {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) => {
                self.defer(format!("{}: {error}", directory.display()));
                return;
            }
        };
        for result in entries {
            let entry = match result {
                Ok(entry) => entry,
                Err(error) => {
                    self.defer(format!("{}: {error}", directory.display()));
                    continue;
                }
            };
            if !eligible_name(&entry.file_name()) {
                continue;
            }
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    self.defer(format!("{}: {error}", path.display()));
                    continue;
                }
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                self.discover(&path, candidates);
                continue;
            }
            if !path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("bam"))
            {
                continue;
            }
            if let Err(error) = crate::text::check_path(&path) {
                self.defer(error.to_string());
                continue;
            }
            match Fingerprint::read(&path) {
                Ok(before) => {
                    if !self
                        .files
                        .get(&path)
                        .is_some_and(|old| old.fingerprint == before)
                    {
                        candidates.push((path, before));
                    }
                }
                Err(error) => self.defer(format!("{}: {error}", path.display())),
            }
        }
    }

    /// Records a discovery error without stopping other files from progressing.
    fn defer(&mut self, message: String) {
        self.snapshot.pending = self.snapshot.pending.saturating_add(1);
        self.snapshot.warning = Some(message);
    }

    /// Commits metadata and totals together, only if the scan's input is stable.
    fn accept(&mut self, path: &Path, before: Fingerprint, stats: Vec<Stats>) -> Result<()> {
        ensure(
            Fingerprint::read(path)? == before,
            "file changed while scanning; retrying next minute",
        )?;
        let old = self.files.get(path);
        let mut totals = Vec::with_capacity(stats.len());
        for (index, (total, new)) in self.snapshot.stats.iter().zip(&stats).enumerate() {
            let previous = old
                .and_then(|file| file.stats.get(index))
                .copied()
                .unwrap_or_default();
            totals.push(total.replace(previous, *new)?);
        }
        let _previous = self.files.insert(
            path.to_path_buf(),
            Contribution {
                fingerprint: before,
                stats,
            },
        );
        self.snapshot.stats = totals;
        self.snapshot.processed = self.files.len();
        Ok(())
    }
}

/// Prunes known working directories and temporary or hidden output names.
#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "The filename is converted to ASCII lowercase before suffix comparisons"
)]
fn eligible_name(file_name: &OsStr) -> bool {
    let name = file_name.to_string_lossy().to_ascii_lowercase();
    !name.starts_with('.')
        && !matches!(name.as_str(), "tmp" | "temp" | "queued_reads")
        && !name.ends_with(".tmp")
        && !name.ends_with(".partial")
        && !name.ends_with(".part")
        && !name.ends_with(".tmp.bam")
        && !name.ends_with(".partial.bam")
        && !name.ends_with(".part.bam")
}

/// Runs nanalogue read-stats on each indexed BED region with primary-only filtering.
fn scan(path: &Path, regions: &[Region]) -> Result<Vec<Stats>> {
    check_eof(path)?;
    let mut reader = nanalogue_indexed_bam_reader(path, FetchDefinition::All)?;
    reader.set_threads(2)?;
    ensure(
        reader.header().target_count() > 0,
        "BAM has no reference sequences; enable MinKNOW output alignment",
    )?;
    let mut stats = vec![Stats::default(); regions.len()];
    for (region, total) in regions.iter().zip(&mut stats) {
        let Some(tid) = reader.header().tid(region.contig.as_bytes()) else {
            continue;
        };
        let interval = GenomicBed3::new(i32::try_from(tid)?, region.start, region.end)?;
        reader.fetch((tid, region.start, region.end))?;
        let options = InputBamBuilder::default()
            .read_filter("primary_forward,primary_reverse".to_owned())
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
fn parse_read_stats(report: &[u8]) -> Result<Stats> {
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
    Ok(Stats {
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
mod tests;
