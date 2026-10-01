//! Recursive BAM discovery and transactional per-file statistics replacement.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use nanalogue_core::bedrs::Intersect as _;
use nanalogue_core::{CurrRead, GenomicBed3, GenomicStrandedBed3, ReadState, nanalogue_bam_reader};
use rust_htslib::bam::Read as _;

use crate::bed::Region;
use crate::error::{Context as _, Result, ensure};

/// Exact sufficient statistics; means are computed only for display.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stats {
    /// Number of primary mapped reads overlapping this region.
    pub count: u64,
    /// Sum of full stored sequence lengths, including soft clipping.
    pub bases: u64,
}

impl Stats {
    /// Accumulates one read without silently wrapping counters.
    fn add_read(&mut self, length: u64) -> Result<()> {
        self.count = self.count.checked_add(1).context("read count overflow")?;
        self.bases = self
            .bases
            .checked_add(length)
            .context("base count overflow")?;
        Ok(())
    }

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
    /// File size in bytes.
    size: u64,
    /// Highest-resolution modification time exposed by the filesystem.
    modified: SystemTime,
}

impl Fingerprint {
    /// Reads the metadata used both before and after scanning.
    fn read(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)?;
        Ok(Self {
            size: metadata.len(),
            modified: metadata.modified()?,
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
    /// Time at which a contribution was last accepted.
    pub updated: Option<Instant>,
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
            updated: None,
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
                Ok(()) => self.snapshot.pending = self.snapshot.pending.saturating_sub(1),
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
        self.snapshot.updated = Some(Instant::now());
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

/// Uses nanalogue's read model and interval intersection without parsing modifications.
fn scan(path: &Path, regions: &[Region]) -> Result<Vec<Stats>> {
    check_eof(path)?;
    let mut reader = nanalogue_bam_reader(path)?;
    ensure(
        reader.header().target_count() > 0,
        "BAM has no reference sequences; enable MinKNOW output alignment",
    )?;
    // An absent contig contributes zero; numeric target IDs are local to each BAM.
    let targets = regions
        .iter()
        .map(|region| {
            reader
                .header()
                .tid(region.contig.as_bytes())
                .map(|tid| -> Result<GenomicBed3> {
                    Ok(GenomicBed3::new(
                        i32::try_from(tid)?,
                        region.start,
                        region.end,
                    )?)
                })
                .transpose()
        })
        .collect::<Result<Vec<_>>>()?;
    let mut stats = vec![Stats::default(); regions.len()];
    for result in reader.records() {
        let record = result?;
        let read = CurrRead::default().set_read_state_and_id(&record)?;
        if !matches!(
            read.read_state(),
            ReadState::PrimaryFwd | ReadState::PrimaryRev
        ) {
            continue;
        }
        // Populate only the data we need, after filtering out non-primary reads
        // (secondary alignments can legitimately omit their stored sequence).
        let aligned = read
            .set_seq_len(&record)?
            .set_align_len(&record)?
            .set_contig_id_and_start(&record)?;
        let span = GenomicStrandedBed3::try_from(&aligned)?;
        let length = u64::from(aligned.seq_len()?);
        for (target, total) in targets.iter().zip(&mut stats) {
            if target.is_some_and(|interval| interval.intersect(&span).is_some()) {
                total.add_read(length)?;
            }
        }
    }
    Ok(stats)
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
