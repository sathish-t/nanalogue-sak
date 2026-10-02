# Adaptive sampling monitor

A terminal dashboard that watches MinKNOW's BAM output and shows, live, how many
reads have landed in each of your target regions and how long they are. It is
built on [nanalogue](https://github.com/DNAReplicationLab/nanalogue), which does
the read filtering and statistics; this crate adds the file watching, the
per-file bookkeeping and the display.

## Quick start

Run it straight from the workspace:

```sh
cargo run --release -p nanalogue_adaptive_sampling_monitor -- targets.bed /path/to/run/bam_pass
```

Or install the binary once and call it directly:

```sh
cargo install --locked --path adaptive_sampling_monitor
nanalogue_adaptive_sampling_monitor targets.bed /path/to/run
```

The command line is deliberately minimal: exactly two paths, a BED file and a
directory, or a standalone `-h`/`--help` or `-V`/`--version`. If either path
happens to begin with `-`, put `--` in front of both. Anything else exits with
status 1.

## What you see

Each BED region gets one row with a horizontal, logarithmic **read-count** bar
drawn from solid Unicode blocks with eighth-cell tips. The exact figures sit
just after the bar, for example `1,547 reads | mean 2,099.9 bp`, where the mean
is the read-count-weighted mean full read length. Rows keep the order of the BED
file. A region with no reads shows an empty bar and `-` in place of a mean;
regions with reads share a single log10 axis with decade labels, and that axis
stays the same for every row even as you scroll, so bars are always comparable.

You will want a Unicode-capable terminal of at least 72 columns by 11 rows. The
title, the quiet ruler and the footer reuse the nanalogue BAM viewer's terminal
styling, and frames are painted with synchronized updates to avoid tearing.

Navigation is what you would expect: **Up/Down** or **j/k** move one row,
**Page Up/Page Down** move a page, **Home/End** jump to the ends, and **q** or
**Ctrl-C** quits and restores the terminal.

When something goes wrong with a file, say a missing index or a BAM that is
still being written, the row just above the footer turns yellow and shows the
most recent problem from the current scan cycle. The display keeps running and
the file is retried next time round.

## Inputs

### The BED file

The BED needs at least four **tab-separated** columns. Column four holds the
region name, which must be at most 40 characters and unique across the whole
file. A bad label or a duplicate name is a fatal startup error that reports the
offending line number.

Blank lines, `#` comments and `track`/`browser` metadata lines are ignored, as
are any extra columns, including strand. The file may be at most 100 kB
(100,000 bytes).

### The BAM files

The BAMs must already be **aligned** to the same reference as the BED, with
contig names matching exactly (`chr1` and `1` are different contigs). Note that
MinKNOW's alignment output has to be switched on separately; enabling adaptive
sampling on its own does not guarantee aligned BAMs. A BAM that carries no
reference sequences is reported as pending, with an explanation.

Every BAM needs an adjacent BAI or CSI index under a standard filename that
HTSlib can discover on its own. MinKNOW writes one next to its
[aligned BAM output](https://software-docs.nanoporetech.com/output-specifications/26.01/read_formats/bam/).

Because this is a real-time monitor that expects modest batches, any BAM larger
than 5 GB (5,000,000,000 bytes) is treated as a fatal input error. If you hit
this, shorten MinKNOW's output batching interval.

## How reads are counted

Only **primary forward and primary reverse** records count, selected with
nanalogue's read-stats filter. There is no extra mapping-quality or pass/fail
filtering, and bear in mind that MinKNOW's `bam_fail` folder is a different
thing from the SAM QC-failed flag.

A read counts toward a region if the alignment's reference span overlaps the
half-open BED interval at all. Deletions and reference skips are part of that
span; insertions and soft clips do not extend it. A single read can therefore
count in several regions.

The mean length shown for a region is the average full sequence length of
every read counted there, across all BAMs, weighted by read count. Soft-clipped
bases are included; hard-clipped bases are not, since the BAM does not store
them.

Read IDs are not deduplicated across files, so point the monitor at a single
set of outputs. A parent directory that also contains copies, merged BAMs or
reanalysed versions of the same reads will count them more than once.

## How this uses nanalogue

The scanner opens each BAM with `nanalogue_indexed_bam_reader` and gives it two
HTSlib decompression threads. For each BED interval it fetches only the indexed
region, configures `InputBam` with that interval and the
`primary_forward,primary_reverse` filter, hands the filtered records to
`read_stats::run`, and reads `n_primary_alignments` and `seq_len_mean` out of
the resulting report. It never parses records, CIGAR strings or modification
tags itself. Target IDs are resolved against each BAM's own header, and a contig
that is absent from a header simply contributes zero.

What lives in this crate is the per-file replacement cache, the weighted
aggregation of read-stats reports and the terminal presentation. The direct
rust-htslib dependency is there for the indexed reader traits, diagnostic
suppression and the test-fixture writing needed around nanalogue's public API;
read filtering and statistics remain nanalogue's job.

## File monitoring

At startup the monitor scans every BAM it can find. After each scan cycle it
waits **60 seconds**, then walks the directory recursively again looking for new
or changed files. The wait starts when a cycle finishes, not when it began, so a
slow cycle simply delays the next one; cycles never overlap. Scanning itself is
synchronous, but between files the display refreshes and still responds to
navigation, `q` and Ctrl-C. Point it at
`bam_pass` if you only want passing reads, or at the parent run directory to
include pass and fail outputs plus any barcode subdirectories.

The directory walk skips hidden entries, MinKNOW's temporary and working
folders (`tmp`, `temp`, `queued_reads`) and anything named like a temporary
file (`.tmp`, `.partial`, `.part`). Symbolic links, whether to files or
directories, are ignored rather than followed. Point the monitor at a
final-output directory rather than a temporary one.

A BAM that is still being written is recognised by its content, not by its age:
there is no rule that holds back files newer than some interval. Before a BAM
is opened, the monitor checks that it ends with the standard BAM end-of-file
marker; a file that is still growing lacks it and is retried on the next cycle.
It also records the file's size and modification time before scanning and
again afterwards, and discards the result if they differ. Nothing is remembered
about a file until a scan of it succeeds, so a failed attempt leaves no trace
for the next cycle to trip over. There are no content hashes or stronger
integrity guarantees.

For every successfully processed file the monitor keeps that size and timestamp
alongside its per-region counts and length totals. If the file later changes,
the next scan **replaces** its earlier contribution; the same file is never
added twice, and while a file is unreadable its last accepted contribution stays
on screen.

If a file that has already been processed disappears, the monitor exits with an
error rather than risk counting the same BAM again under a new path. Nothing is
persisted between launches, and memory grows with the number of file/region
combinations, each holding a count and a length sum.

MinKNOW [writes each batch to a temporary folder and moves it into the final output folder](https://nanoporetech.com/support/software/MinKNOW/post-run-options/why-do-my-reads-end-up-in-the-pod5-skip-queued-reads-or-tmp-folder)
only once it is complete. A BAM that has appeared in `bam_pass` is therefore
already finished, so the monitor counts it straight away instead of waiting for
the next batch to arrive before trusting it. Keep in mind that MinKNOW's
[default BAM batch duration is one hour](https://software-docs.nanoporetech.com/output-specifications/26.01/read_formats/bam/);
if you want more frequent updates, choose 1- or 10-minute basecalled output
batching in MinKNOW, since polling more often cannot surface reads that MinKNOW
has not yet published.
