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
Solid Unicode blocks with eighth-cell tips form the bars; exact counts and mean
full read lengths float directly after each bar, e.g. `1,547 reads | mean 2,099.9 bp`.
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
  Extra BED columns, including strand, are ignored.
- BAMs must already be **aligned** to the BED's reference. Contig names must match
  exactly (`chr1` and `1` are different). MinKNOW output alignment needs to be
  enabled separately; adaptive sampling alone should not be assumed to supply it.
  A BAM without reference sequences is reported as pending with an explanation.
- Only **primary mapped** records count, classified through nanalogue's `CurrRead`
  and `ReadState`. There is no additional mapping-quality or pass/fail filter.
  Nanalogue's validation applies: unsupported paired/mate, duplicate or QC-failed
  flags, invalid flag combinations and malformed primary alignments make the BAM
  pending with a warning, retaining its previous contribution rather than accepting
  partial statistics. MinKNOW's `bam_fail` folder is not the SAM QC-failed flag.
- Any overlap between the half-open BED interval and the alignment's reference
  span counts. Deletions and reference skips are part of that span; insertions
  and soft clips do not extend it. A read can count in several BED regions.
- Mean length is the sum of stored sequence lengths divided by the read count.
  It includes soft-clipped bases, excludes hard-clipped bases not stored in BAM,
  and is rounded to one decimal for display. Nanalogue rejects missing sequences
  on primary mapped records; excluded secondary/supplementary/unmapped records
  need not contain a sequence.
- No BAM index is required. Read IDs are not deduplicated across files: point at
  one set of outputs, not a parent containing both original BAMs and their copies,
  merged BAMs, or reanalysed versions.

### How this uses nanalogue

The scanner opens BAMs with `nanalogue_bam_reader`, classifies records with
`CurrRead::set_read_state_and_id`, and loads sequence length and alignment data
through `CurrRead` setters. `GenomicStrandedBed3::try_from(&read)` supplies the
reference span; nanalogue's `bedrs::Intersect` tests each BED target for overlap.
It does not parse modification tags or implement its own CIGAR or overlap logic.
Target IDs are resolved separately for each BAM header; absent contigs stay zero.

Only the streaming count/length-sum accumulators, per-file replacement cache and
terminal presentation live here. Nanalogue's `read_stats` is a report-producing
API (including median and N50), not a structured per-BED aggregate API. The direct
rust-htslib dependency provides the reader trait/header access required by
nanalogue's public API, diagnostic suppression and test-fixture writing; read
interpretation belongs to nanalogue. Scanning uses one BAM pass and checks each
primary read against the BED targets.

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

Each successfully processed path retains its size, modification timestamp and
per-region counts/length sums. If its metadata changes, a new scan **replaces**
its previous contribution; it never adds the same file twice. Results are
accepted only after a successful scan and matching before/after metadata.
Incomplete/unreadable files are retried next cycle while their last accepted
contribution remains visible. A standard 28-byte BAM end marker is required;
there are no content hashes or integrity guarantees. The status row shows the
most recent problem in the current cycle.

Totals are cumulative for this session: if a processed file disappears, its
accepted contribution is retained. State is not persisted between launches.
Memory usage includes a count and length sum for every file/region combination.

MinKNOW [documents temporary files being moved to final output folders](https://nanoporetech.com/support/software/MinKNOW/post-run-options/why-do-my-reads-end-up-in-the-pod5-skip-queued-reads-or-tmp-folder).
The monitor therefore includes the newest finalised BAM rather than holding
back the highest numbered file. MinKNOW's [default BAM batch duration is one hour](https://software-docs.nanoporetech.com/output-specifications/26.01/read_formats/bam/).
Choose 1- or 10-minute basecalled output batching in MinKNOW for more frequent
updates; polling cannot make unpublished reads available sooner.

## Building and checking

Use current stable Rust with Clippy and rustfmt. The native HTSlib dependencies
need a C compiler, CMake, pkg-config, OpenSSL, zlib, bzip2, xz/liblzma and libclang.
For Debian 12, a suitable setup is:

```sh
sudo apt-get install build-essential cmake pkg-config clang-19 libclang-19-dev \
  libssl-dev zlib1g-dev libbz2-dev liblzma-dev
rustup component add clippy rustfmt
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

Nanalogue is pinned to a revision of its `main` branch. All upstream Cargo lint
settings are copied unchanged into `[workspace.lints]`; each member uses
`[lints] workspace = true`. New projects should inherit these settings too.
The lockfile is shared and tracked. Tests generate small, unindexed BAM fixtures
locally and need neither MinKNOW nor sequencing hardware.
