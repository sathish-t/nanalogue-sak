# nanalogue-sak

A Swiss army knife of Rust tools built on [nanalogue](https://github.com/DNAReplicationLab/nanalogue).
Each tool lives in its own Cargo package within this workspace, not a nested Git repository.

## Adaptive sampling monitor

```sh
cargo run --release -p nanalogue_adaptive_sampling_monitor -- targets.bed /path/to/run/bam_pass
```

Or install the binary:

```sh
cargo install --locked --path adaptive_sampling_monitor
nanalogue_adaptive_sampling_monitor targets.bed /path/to/run
```

Arguments are parsed with Rust's standard library: exactly two paths, or a
standalone `-h`/`--help` or `-V`/`--version`. Prefix both paths with `--` if either
begins with `-`. Invalid invocations exit with status 1. The monitor does not use
Clap directly; nanalogue still brings it in as a transitive dependency.

The terminal shows one horizontal logarithmic **read-count** bar per BED region.
Solid Unicode blocks with eighth-cell tips form the bars; exact counts and weighted
mean full read lengths float directly after each bar, e.g. `1,547 reads | mean 2,099.9 bp`.
Rows stay in BED order. Zero counts have an empty bar and `-` for their mean;
positive counts use a log10 axis with decade labels. The scale is shared across
all rows, even when scrolling. Use a Unicode-capable terminal at least 72 columns
by 11 rows. The title, quiet ruler and footer borrow the nanalogue BAM viewer's
terminal styling; frames are painted with synchronized updates.

Controls: **Up/Down** or **j/k**, **Page Up/Page Down**, **Home/End**;
**q** or **Ctrl-C** exits and restores the terminal.

All displayed text is printable ASCII (space through `~`); only the generated
block bars use Unicode. External errors are escaped before display and clipping,
so Unicode and terminal control characters cannot change the layout. No Unicode
width library is needed. Supplied BED and directory paths must be printable ASCII;
invalid paths are rejected before opening the live screen. Discovered BAM paths
(including parent directory names) must also be printable ASCII; invalid paths
are skipped and reported as pending. Filenames have no 40-character limit.
This adds no validation of BAM read IDs beyond nanalogue's existing checks.

### Inputs and counting

- BED must contain at least four **tab-separated** columns. Names in column four
  must be nonempty printable ASCII, at most 40 characters and unique across the
  entire file. Contig names must also be nonempty printable ASCII without leading
  or trailing whitespace. Invalid labels and duplicates are fatal startup errors
  with a line number.
  Coordinates must satisfy `0 <= start < end <= u32::MAX`,
  matching nanalogue's coordinate representation.
  Blank lines, `#` comments and `track`/`browser` metadata lines are ignored.
  Extra BED columns, including strand, are ignored. The BED file may be at most
  100 kB (100,000 bytes).
- BAMs must already be **aligned** to the BED's reference. Contig names must match
  exactly (`chr1` and `1` are different). MinKNOW output alignment needs to be
  enabled separately; adaptive sampling alone should not be assumed to supply it.
  A BAM without reference sequences is reported as pending with an explanation.
  Each BAM must have an adjacent BAI or CSI index using a standard filename that
  HTSlib can discover. MinKNOW produces an index alongside its
  [aligned BAM output](https://software-docs.nanoporetech.com/output-specifications/26.01/read_formats/bam/).
  BAMs larger than 5 GB (5,000,000,000 bytes) are fatal input errors because this
  real-time monitor expects smaller batches; decrease MinKNOW's output batching
  interval if needed.
- Only **primary forward and primary reverse** records count, selected with
  nanalogue's read-stats filter. There is no additional mapping-quality or
  pass/fail filter. MinKNOW's `bam_fail` folder is not the SAM QC-failed flag.
- Any overlap between the half-open BED interval and the alignment's reference
  span counts. Deletions and reference skips are part of that span; insertions
  and soft clips do not extend it. A read can count in several BED regions.
- Nanalogue read-stats reports an integer `seq_len_mean` for each BAM and region.
  The monitor reconstructs that file's length total as
  `seq_len_mean * n_primary_alignments`, then divides the sum of those totals by
  the total primary count. Thus the displayed cross-BAM mean is weighted by read
  count, but inherits read-stats' per-file integer truncation before it is rounded
  to one decimal for display. Sequence length includes soft-clipped bases and
  excludes hard-clipped bases not stored in BAM.
- Read IDs are not deduplicated across files: point at one set of outputs, not a
  parent containing both original BAMs and their copies, merged BAMs, or
  reanalysed versions.

### How this uses nanalogue

The scanner opens each BAM with `nanalogue_indexed_bam_reader` and assigns two
HTSlib decompression threads. For each BED interval it fetches only the indexed
region, configures `InputBam` with that interval and the
`primary_forward,primary_reverse` filter, passes the filtered records to
`read_stats::run`, and extracts `n_primary_alignments` and `seq_len_mean` from its
report. It does not parse records, CIGAR strings or modification tags itself.
Target IDs are resolved separately for each BAM header; absent contigs stay zero.

The per-file replacement cache, weighted aggregation of read-stats reports and
terminal presentation live here. The direct rust-htslib dependency provides the
indexed reader traits, diagnostic suppression and test-fixture writing required
around nanalogue's public API; read filtering and statistics belong to nanalogue.

### File monitoring

Existing BAMs are scanned at startup. After each scan cycle, the monitor waits
**60 seconds** before checking recursively for new or changed files. Scanning is
synchronous, so keyboard input and redraws pause while BAMs are being processed.
Point at `bam_pass` for passing reads only, or a parent directory to include both
pass and fail outputs and barcode subdirectories.

Hidden entries, `tmp`, `temp`, `queued_reads`, and names ending in `.tmp`,
`.partial`, `.part`, `.tmp.bam`, `.partial.bam` or `.part.bam` are skipped below
the supplied directory. Symlink entries are not followed. Use a final-output
directory rather than explicitly selecting a temporary directory as the root.

Each successfully processed path retains the BAM's size and modification
timestamp, plus per-region counts and reconstructed length totals. If the BAM's
metadata changes, a new scan **replaces** its previous contribution; it never
adds the same file twice. Results are accepted only after a successful scan and
matching before/after BAM metadata. HTSlib discovers and loads the index.
Incomplete/unreadable files are retried next cycle while their last accepted
contribution remains visible. A standard 28-byte BAM end marker is required;
there are no content hashes or integrity guarantees. The status row shows the
most recent problem in the current cycle.

If a processed file disappears, the monitor exits with an error rather than risk
counting the same BAM again under a new path. State is not persisted between
launches. Memory usage includes a count and length sum for every file/region
combination.

MinKNOW [documents temporary files being moved to final output folders](https://nanoporetech.com/support/software/MinKNOW/post-run-options/why-do-my-reads-end-up-in-the-pod5-skip-queued-reads-or-tmp-folder).
The monitor therefore includes the newest finalised BAM rather than holding
back the highest numbered file. MinKNOW's [default BAM batch duration is one hour](https://software-docs.nanoporetech.com/output-specifications/26.01/read_formats/bam/).
Choose 1- or 10-minute basecalled output batching in MinKNOW for more frequent
updates; polling cannot make unpublished reads available sooner.

## Building and checking

Rust 1.99.0, Clippy and rustfmt are pinned in `rust-toolchain.toml`; rustup selects
and installs that toolchain automatically. Upgrade the pin deliberately together
with any resulting lint fixes. The native HTSlib dependencies need a C compiler,
CMake, pkg-config, OpenSSL, zlib, bzip2, xz/liblzma and libclang. For Debian 12, a
suitable setup is:

```sh
sudo apt-get install build-essential cmake pkg-config clang-19 libclang-19-dev \
  libssl-dev zlib1g-dev libbz2-dev liblzma-dev
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

Nanalogue is pinned to a revision of its `main` branch. All upstream Cargo lint
settings are copied unchanged into `[workspace.lints]`; each member uses
`[lints] workspace = true`. New projects should inherit these settings too.
The lockfile is shared and tracked. Tests generate small, coordinate-sorted and
indexed BAM fixtures locally and need neither MinKNOW nor sequencing hardware.
