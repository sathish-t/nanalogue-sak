//! Recursive BAM discovery and transactional per-file statistics replacement.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[cfg(test)]
use crate::bam_region_scan::parse_read_stats;
use crate::bam_region_scan::scan;
use crate::bed::Region;
use crate::error::{Context as _, Result, ensure};

/// Upper bound for BAM batches suitable for responsive real-time monitoring.
const MAX_BAM_BYTES: u64 = 5_000_000_000;

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
                .context("read count arithmetic failed")?,
            bases: self
                .bases
                .checked_sub(old.bases)
                .and_then(|value| value.checked_add(new.bases))
                .context("base count arithmetic failed")?,
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
}

impl Fingerprint {
    /// Reads BAM metadata around each scan; `HTSlib` owns index discovery.
    fn read(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)?;
        Ok(Self {
            bam_size: metadata.len(),
            bam_modified: metadata.modified()?,
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

    /// Discovers changes, reporting cooperative UI checkpoints during synchronous scans.
    pub(crate) fn refresh<F>(&mut self, mut progress: F) -> Result<ControlFlow<()>>
    where
        F: FnMut(&Snapshot) -> Result<ControlFlow<()>>,
    {
        self.ensure_processed_files_exist()?;
        self.snapshot.warning = None;
        self.snapshot.pending = 0;
        "Discovering BAM files".clone_into(&mut self.snapshot.activity);
        if progress(&self.snapshot)?.is_break() {
            return Ok(ControlFlow::Break(()));
        }
        let mut candidates = Vec::new();
        let directory = self.directory.clone();
        self.discover(&directory, &mut candidates);
        candidates.sort_by(|left, right| left.0.cmp(&right.0));
        self.snapshot.pending = self.snapshot.pending.saturating_add(candidates.len());
        let candidate_count = candidates.len();
        for (index, (path, before)) in candidates.into_iter().enumerate() {
            ensure(
                before.bam_size <= MAX_BAM_BYTES,
                format!(
                    "BAM exceeds the 5 GB size limit ({} bytes): {}. This real-time monitor expects smaller BAM batches; decrease the output batching interval in MinKNOW",
                    before.bam_size,
                    path.display()
                ),
            )?;
            self.snapshot.activity = format!(
                "Scanning BAM {}/{}: {}",
                index.saturating_add(1),
                candidate_count,
                path.display()
            );
            if progress(&self.snapshot)?.is_break() {
                return Ok(ControlFlow::Break(()));
            }

            let result =
                scan(&path, &self.regions).and_then(|stats| self.accept(&path, before, stats));
            if let Err(error) = result {
                self.snapshot.warning = Some(format!("{}: {error}", path.display()));
            } else {
                self.snapshot.pending = self
                    .snapshot
                    .pending
                    .checked_sub(1)
                    .context("pending BAM count underflow")?;
            }
            if progress(&self.snapshot)?.is_break() {
                return Ok(ControlFlow::Break(()));
            }
        }
        "Watching; checking for changes every 60s".clone_into(&mut self.snapshot.activity);
        Ok(ControlFlow::Continue(()))
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

#[cfg(test)]
mod tests;
