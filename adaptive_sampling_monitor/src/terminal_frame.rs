//! Pure terminal-frame layout, logarithmic bars and display formatting.

use crossterm::style::{Attribute, Color, StyledContent, Stylize as _};

use crate::bed::Region;
#[cfg(test)]
use crate::error::Result;
use crate::monitor::{MonitorSnapshot, RegionReadStats};
use crate::text::escape_terminal_text;

/// Rows reserved for title, statistics, headings, axis and status/footer.
pub(super) const CHROME_ROWS: usize = 9;

/// The viewer's restrained teal, used only for the data and live status.
const TEAL: Color = Color::Rgb {
    r: 138,
    g: 190,
    b: 183,
};

/// Secondary text remains legible without competing with bar-end counts.
const MUTED: Color = Color::Rgb {
    r: 150,
    g: 152,
    b: 150,
};

/// Muted blue background for the application title.
const TITLE_BACKGROUND: Color = Color::Rgb {
    r: 81,
    g: 104,
    b: 130,
};

/// A row of separately styled terminal-native text runs.
pub(super) type Line = Vec<StyledContent<String>>;

/// Clips normalized frame text: printable ASCII plus our one-cell block glyphs.
/// External text must be escaped before entering a frame, not after clipping.
pub(super) fn fit(text: &str, width: usize) -> String {
    text.chars().take(width).collect()
}

/// Pads a validated ASCII label to the requested number of cells.
fn label(text: &str, width: usize) -> String {
    let clipped = fit(text, width);
    let used = clipped.len();
    format!("{clipped}{}", " ".repeat(width.saturating_sub(used)))
}

/// Keeps the error reason at the end of long path-prefixed warnings visible.
fn warning_text(message: &str, columns: usize) -> String {
    let prefix = " Waiting/retry: ";
    let room = columns.saturating_sub(prefix.len());
    let safe = escape_terminal_text(message);
    if safe.len() <= room {
        return format!("{prefix}{safe}");
    }
    let reversed: String = safe.chars().rev().collect();
    let tail: String = fit(&reversed, room.saturating_sub(3))
        .chars()
        .rev()
        .collect();
    format!("{prefix}...{tail}")
}

/// Converts reconstructed integer statistics to display-only floating point.
#[expect(
    clippy::cast_precision_loss,
    reason = "Display rounds the reconstructed weighted mean to one decimal"
)]
fn mean(stats: RegionReadStats) -> String {
    if stats.count == 0 {
        return "-".to_owned();
    }
    let number = format!("{:.1}", stats.bases as f64 / stats.count as f64);
    let (integer, fraction) = number.split_once('.').unwrap_or((&number, "0"));
    format!("{}.{fraction}", grouped(integer))
}

/// Separates thousands without rounding away any count digits.
fn grouped(digits: &str) -> String {
    let mut reversed = String::new();
    for (index, character) in digits.chars().rev().enumerate() {
        if index > 0 && index.is_multiple_of(3) {
            reversed.push(',');
        }
        reversed.push(character);
    }
    reversed.chars().rev().collect()
}

/// Count and reconstructed mean annotations, placed directly after each bar's tip.
fn annotation(stats: RegionReadStats) -> (String, String) {
    let reads = if stats.count == 1 { "read" } else { "reads" };
    let count = format!("{} {reads}", grouped(&stats.count.to_string()));
    let unit = if stats.count == 0 { "" } else { " bp" };
    (count, format!(" | mean {}{unit}", mean(stats)))
}

/// Maps positive counts onto log10 in eighth-cells; singleton reads stay visible.
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "Bounded eighth-cell projection for display; stored counts stay exact"
)]
fn bar_eighths(count: u64, decades: u32, width: usize) -> usize {
    if count == 0 || width == 0 {
        return 0;
    }
    let ratio = (count as f64).log10() / f64::from(decades);
    let eighths = (ratio * width.saturating_sub(1).saturating_mul(8) as f64).round() as usize;
    eighths.saturating_add(8).min(width.saturating_mul(8))
}

/// Solid blocks with a fractional final cell, never dot/ASCII approximations.
fn bar(count: u64, decades: u32, width: usize) -> String {
    /// Unicode eighth-cell fills in increasing order.
    const TIPS: [&str; 8] = [
        "", "\u{258f}", "\u{258e}", "\u{258d}", "\u{258c}", "\u{258b}", "\u{258a}", "\u{2589}",
    ];
    let eighths = bar_eighths(count, decades, width);
    let mut text = "\u{2588}".repeat(eighths.checked_div(8).unwrap_or(0));
    let remainder = eighths.checked_rem(8).unwrap_or(0);
    text.push_str(TIPS.get(remainder).copied().unwrap_or_default());
    text
}

/// Places decade ticks and non-overlapping labels on the shared global scale.
fn axis(decades: u32, width: usize) -> (String, String) {
    let mut labels = vec![' '; width];
    let mut rule = vec!['-'; width];
    let mut available_end = width;
    // Give the upper bound priority when narrow terminals cannot fit every label.
    for exponent in (0..=decades).rev() {
        if let Some(value) = 10u64.checked_pow(exponent) {
            let position = bar_eighths(value, decades, width)
                .div_ceil(8)
                .saturating_sub(1);
            if let Some(cell) = rule.get_mut(position) {
                *cell = '+';
            }
            let tick = if exponent < 4 {
                value.to_string()
            } else {
                format!("1e{exponent}")
            };
            let start = position.min(width.saturating_sub(tick.len()));
            if start.saturating_add(tick.len()) <= available_end {
                for (cell, character) in labels.iter_mut().skip(start).zip(tick.chars()) {
                    *cell = character;
                }
                available_end = start.saturating_sub(1);
            }
        }
    }
    (labels.iter().collect(), rule.iter().collect())
}

/// Shows resize and quit instructions when the histogram cannot fit.
fn small_frame(columns: usize, rows: usize) -> Vec<Line> {
    let mut lines = vec![
        "Terminal too small".to_owned(),
        "Resize to at least 72 x 11".to_owned(),
        "q / Ctrl-C: quit".to_owned(),
    ];
    lines.resize(rows, String::new());
    lines
        .into_iter()
        .map(|line| vec![fit(&line, columns).with(MUTED)])
        .collect()
}

/// Constructs a terminal-sized frame with a global scale across scrolled rows.
pub(super) fn frame(
    regions: &[Region],
    snapshot: &MonitorSnapshot,
    offset: usize,
    width: u16,
    height: u16,
) -> Vec<Line> {
    let columns = usize::from(width).saturating_sub(1);
    let rows = usize::from(height);
    if width < 72 || height < 11 {
        return small_frame(columns, rows);
    }
    let visible = rows.saturating_sub(CHROME_ROWS);
    let end = offset.saturating_add(visible).min(regions.len());
    let name_width = regions
        .iter()
        .map(|region| region.name.len())
        .max()
        .unwrap_or(0)
        .clamp(10, 24.min(columns.checked_div(4).unwrap_or(0)).max(10));
    let annotation_width = snapshot
        .stats
        .iter()
        .map(|stats| {
            let (count, length) = annotation(*stats);
            count.len().saturating_add(length.len())
        })
        .max()
        .unwrap_or(0);
    let graph_width = columns
        .saturating_sub(name_width)
        .saturating_sub(annotation_width)
        .saturating_sub(6);
    let maximum = snapshot
        .stats
        .iter()
        .map(|stats| stats.count)
        .max()
        .unwrap_or(0)
        .max(10);
    let decades = maximum.saturating_sub(1).ilog10().saturating_add(1);
    let (ticks, rule) = axis(decades, graph_width);
    let prefix = " ".repeat(name_width.saturating_add(3));
    let mut lines = vec![
        vec![
            label(" nanalogue / adaptive sampling", columns)
                .white()
                .on(TITLE_BACKGROUND)
                .bold(),
        ],
        vec![
            format!(
                " BAMs: {} processed  |  {} pending  |  poll 60s",
                snapshot.processed, snapshot.pending
            )
            .with(TEAL),
        ],
        Vec::new(),
        vec![format!(" {}  READ COUNT / log10", label("REGION", name_width)).with(MUTED)],
        vec![format!("{prefix}{ticks}").with(MUTED)],
        vec![format!("{prefix}{rule}").with(MUTED)],
    ];
    for (region, stats) in regions
        .iter()
        .zip(&snapshot.stats)
        .skip(offset)
        .take(visible)
    {
        let (count, length) = annotation(*stats);
        lines.push(vec![
            format!(" {}  ", label(&region.name, name_width)).stylize(),
            bar(stats.count, decades, graph_width).with(TEAL),
            format!("  {count}").attribute(if stats.count == 0 {
                Attribute::NormalIntensity
            } else {
                Attribute::Bold
            }),
            length.with(MUTED),
        ]);
    }
    lines.resize(rows.saturating_sub(3), Vec::new());
    lines.push(vec![
        format!(" {}", escape_terminal_text(&snapshot.activity)).with(MUTED),
    ]);
    lines.push(vec![snapshot.warning.as_ref().map_or_else(
        || {
            format!(
                " Regions {}-{} / {}  |  primary mapped reads  |  full read length",
                offset.saturating_add(1),
                end,
                regions.len()
            )
            .with(MUTED)
        },
        |warning| warning_text(warning, columns).with(Color::Yellow),
    )]);
    lines.push(vec![
        label(
            " j/k up/down scroll   pgup/pgdn   home/end   q quit",
            columns,
        )
        .reverse(),
    ]);
    lines
}

#[cfg(test)]
mod tests {
    //! Display scale, weighting and terminal-width regressions.
    use super::*;
    use crate::error::Context as _;

    /// Projects styled runs to their plain terminal text without ANSI escapes.
    fn text(line: &Line) -> String {
        line.iter().map(|span| span.content().as_str()).collect()
    }

    /// Serializes complete frame content and styling for golden characterization.
    fn frame_signature(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|line| {
                line.iter()
                    .map(|span| format!("{:?}:{:?}", span.style(), span.content()))
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Locks down a complete representative frame before code is reorganized.
    #[test]
    fn terminal_frame_golden_characterization() -> Result<()> {
        let regions = crate::bed::parse_bed_regions(
            &b"chr1\t0\t10\talpha\nchr1\t10\t20\tbeta\nchr1\t20\t30\tgamma\n"[..],
        )?;
        let mut snapshot = MonitorSnapshot::new(3);
        snapshot.stats = vec![
            RegionReadStats {
                count: 100,
                bases: 123_400,
            },
            RegionReadStats { count: 1, bases: 9 },
            RegionReadStats::default(),
        ];
        snapshot.processed = 2;
        snapshot.pending = 1;
        snapshot.activity = "Scanning BAM 2/3: run/reads.bam".to_owned();
        snapshot.warning = Some("run/pending.bam: missing index".to_owned());

        let actual = frame_signature(&frame(&regions, &snapshot, 0, 72, 12));
        let expected = concat!(
            "ContentStyle { foreground_color: Some(White), background_color: Some(Rgb { r: 81, g: 104, b: 130 }), underline_color: None, attributes: Attributes(4) }:\" nanalogue / adaptive sampling                                         \"\n",
            "ContentStyle { foreground_color: Some(Rgb { r: 138, g: 190, b: 183 }), background_color: None, underline_color: None, attributes: Attributes(0) }:\" BAMs: 2 processed  |  1 pending  |  poll 60s\"\n",
            "\n",
            "ContentStyle { foreground_color: Some(Rgb { r: 150, g: 152, b: 150 }), background_color: None, underline_color: None, attributes: Attributes(0) }:\" REGION      READ COUNT / log10\"\n",
            "ContentStyle { foreground_color: Some(Rgb { r: 150, g: 152, b: 150 }), background_color: None, underline_color: None, attributes: Attributes(0) }:\"             1             10         100\"\n",
            "ContentStyle { foreground_color: Some(Rgb { r: 150, g: 152, b: 150 }), background_color: None, underline_color: None, attributes: Attributes(0) }:\"             +-------------+------------+\"\n",
            "ContentStyle { foreground_color: None, background_color: None, underline_color: None, attributes: Attributes(0) }:\" alpha       \"|ContentStyle { foreground_color: Some(Rgb { r: 138, g: 190, b: 183 }), background_color: None, underline_color: None, attributes: Attributes(0) }:\"████████████████████████████\"|ContentStyle { foreground_color: None, background_color: None, underline_color: None, attributes: Attributes(4) }:\"  100 reads\"|ContentStyle { foreground_color: Some(Rgb { r: 150, g: 152, b: 150 }), background_color: None, underline_color: None, attributes: Attributes(0) }:\" | mean 1,234.0 bp\"\n",
            "ContentStyle { foreground_color: None, background_color: None, underline_color: None, attributes: Attributes(0) }:\" beta        \"|ContentStyle { foreground_color: Some(Rgb { r: 138, g: 190, b: 183 }), background_color: None, underline_color: None, attributes: Attributes(0) }:\"█\"|ContentStyle { foreground_color: None, background_color: None, underline_color: None, attributes: Attributes(4) }:\"  1 read\"|ContentStyle { foreground_color: Some(Rgb { r: 150, g: 152, b: 150 }), background_color: None, underline_color: None, attributes: Attributes(0) }:\" | mean 9.0 bp\"\n",
            "ContentStyle { foreground_color: None, background_color: None, underline_color: None, attributes: Attributes(0) }:\" gamma       \"|ContentStyle { foreground_color: Some(Rgb { r: 138, g: 190, b: 183 }), background_color: None, underline_color: None, attributes: Attributes(0) }:\"\"|ContentStyle { foreground_color: None, background_color: None, underline_color: None, attributes: Attributes(131072) }:\"  0 reads\"|ContentStyle { foreground_color: Some(Rgb { r: 150, g: 152, b: 150 }), background_color: None, underline_color: None, attributes: Attributes(0) }:\" | mean -\"\n",
            "ContentStyle { foreground_color: Some(Rgb { r: 150, g: 152, b: 150 }), background_color: None, underline_color: None, attributes: Attributes(0) }:\" Scanning BAM 2/3: run/reads.bam\"\n",
            "ContentStyle { foreground_color: Some(Yellow), background_color: None, underline_color: None, attributes: Attributes(0) }:\" Waiting/retry: run/pending.bam: missing index\"\n",
            "ContentStyle { foreground_color: None, background_color: None, underline_color: None, attributes: Attributes(4096) }:\" j/k up/down scroll   pgup/pgdn   home/end   q quit                    \"",
        );
        assert_eq!(
            actual, expected,
            "complete frame content and styles stay stable"
        );
        Ok(())
    }

    /// Zero and singleton counts must not disappear into the same log position.
    #[test]
    fn logarithmic_scale_and_weighted_mean() {
        assert_eq!(bar(0, 3, 31), "", "zero has no bar");
        assert_eq!(bar(1, 3, 31), "\u{2588}", "one is visible");
        assert_eq!(bar_eighths(10, 3, 31), 88, "ten is one decade along");
        assert_eq!(bar_eighths(100, 3, 31), 168, "hundred is two decades along");
        assert_eq!(
            bar(1000, 3, 31),
            "\u{2588}".repeat(31),
            "maximum fills the graph"
        );
        assert_eq!(
            bar(2, 1, 9),
            "\u{2588}\u{2588}\u{2588}\u{258d}",
            "log10(2) gives three full cells and a three-eighths tip"
        );
        assert_eq!(
            mean(RegionReadStats {
                count: 3,
                bases: 401
            }),
            "133.7",
            "round the weighted mean only for display"
        );
        assert_eq!(
            mean(RegionReadStats::default()),
            "-",
            "empty regions have no mean"
        );
        assert_eq!(
            annotation(RegionReadStats {
                count: 1547,
                bases: 3_248_600
            }),
            ("1,547 reads".to_owned(), " | mean 2,099.9 bp".to_owned()),
            "exact counts and rounded means use readable thousands separators"
        );
        let (ticks, rule) = axis(4, 26);
        assert_eq!(
            ticks.split_whitespace().collect::<Vec<_>>(),
            vec!["1", "10", "100", "1e4"],
            "narrow axes keep the upper bound instead of overlapping labels"
        );
        assert_eq!(
            rule.chars().filter(|character| *character == '+').count(),
            5,
            "all decade ticks remain even when one label is omitted"
        );
    }

    /// External text is escaped before measuring; our block bars stay one cell each.
    #[test]
    fn cell_width_and_control_characters() {
        assert_eq!(
            label("abc", 4),
            "abc ",
            "ASCII label is padded by byte length"
        );
        assert_eq!(
            fit(&escape_terminal_text("a\u{1b}\nb"), 4),
            "a\\u{",
            "escape expansion happens before clipping"
        );
        assert_eq!(
            fit("\u{2588}\u{258d} x", 3),
            "\u{2588}\u{258d} ",
            "block glyphs are measured in cells, not UTF-8 bytes"
        );
        let warning = warning_text(
            "/very/long/path/to/sequencing/run/bam_pass/pending.bam: BAM is incomplete: missing end marker",
            71,
        );
        assert!(
            warning.starts_with(" Waiting/retry: ...")
                && warning.ends_with("BAM is incomplete: missing end marker"),
            "narrow warnings preserve the reason instead of just the directory path"
        );
        assert_eq!(
            warning.len(),
            71,
            "warning occupies only its available cells"
        );
    }

    /// Only generated bars may introduce non-ASCII cells into a rendered frame.
    #[test]
    fn frame_escapes_external_messages() -> Result<()> {
        let regions = crate::bed::parse_bed_regions(&b"chr1\t0\t10\tASCII label\n"[..])?;
        let mut snapshot = MonitorSnapshot::new(1);
        snapshot.stats = vec![RegionReadStats {
            count: 2,
            bases: 13,
        }];
        snapshot.activity = "Scanning \u{754c}\n\u{1b}[31m".to_owned();
        snapshot.warning = Some("read ID: \u{e9}\t\u{2588}".to_owned());
        let lines = frame(&regions, &snapshot, 0, 72, 11);
        for (row, line) in lines.iter().enumerate() {
            for (column, span) in line.iter().enumerate() {
                if row == 6 && column == 1 {
                    assert!(
                        span.content()
                            .chars()
                            .all(|cell| ('\u{2588}'..='\u{258f}').contains(&cell)),
                        "only block glyphs in the bar"
                    );
                } else {
                    assert!(
                        crate::text::is_printable_ascii(span.content()),
                        "all text must be printable ASCII"
                    );
                }
            }
        }
        assert_eq!(
            text(lines.get(8).context("activity row")?),
            " Scanning \\u{754c}\\n\\u{1b}[31m",
            "activity is escaped before drawing"
        );
        assert_eq!(
            text(lines.get(9).context("warning row")?),
            " Waiting/retry: read ID: \\u{e9}\\t\\u{2588}",
            "even bar-like external characters must be escaped"
        );
        Ok(())
    }

    /// Scrolling preserves input order and a compact screen remains bounded.
    #[test]
    fn scrolled_and_small_frames() -> Result<()> {
        let regions = crate::bed::parse_bed_regions(
            &b"chr1\t0\t10\tfirst\nchr1\t10\t20\tsecond\nchr1\t20\t30\tthird\n"[..],
        )?;
        let mut snapshot = MonitorSnapshot::new(3);
        snapshot.warning = Some("incomplete BAM".to_owned());
        let lines = frame(&regions, &snapshot, 1, 80, 11);
        let screen = lines.iter().map(text).collect::<Vec<_>>().join("\n");
        assert!(!screen.contains("first"), "scrolled-out row is hidden");
        assert!(
            screen.contains("second") && screen.contains("third"),
            "BED order survives scrolling"
        );
        assert!(
            screen.contains("incomplete BAM"),
            "pending-file error is visible"
        );
        assert_eq!(lines.len(), 11, "frame fits the height");
        for line in lines {
            assert!(text(&line).chars().count() < 80, "no autowrap");
        }
        let small = frame(&regions, &snapshot, 0, 30, 4);
        assert_eq!(small.len(), 4, "resize fallback fits the height");
        assert!(
            small.iter().any(|line| text(line) == "q / Ctrl-C: quit"),
            "quit instructions remain complete in a narrow terminal"
        );
        Ok(())
    }

    /// Annotations follow tips, not columns, without changing scale on scroll.
    #[test]
    fn floating_annotations_and_global_scale() -> Result<()> {
        let regions = crate::bed::parse_bed_regions(
            &b"chr1\t0\t10\tlong\nchr1\t10\t20\tshort\nchr1\t20\t30\tempty\n"[..],
        )?;
        let mut snapshot = MonitorSnapshot::new(3);
        snapshot.stats = vec![
            RegionReadStats {
                count: 1000,
                bases: 2_000_000,
            },
            RegionReadStats {
                count: 2,
                bases: 13,
            },
            RegionReadStats::default(),
        ];
        for width in [72, 120] {
            let lines = frame(&regions, &snapshot, 0, width, 12);
            let long = text(lines.get(6).context("long row")?);
            let short = text(lines.get(7).context("short row")?);
            let empty = text(lines.get(8).context("zero row")?);
            let long_prefix = long
                .split("1,000 reads")
                .next()
                .context("long annotation")?;
            let short_prefix = short.split("2 reads").next().context("short annotation")?;
            assert!(
                long_prefix.chars().count() > short_prefix.chars().count(),
                "numbers must move with the bar tip"
            );
            assert!(
                short.contains("  2 reads | mean 6.5 bp"),
                "annotation stays immediately after bar, with full mean"
            );
            assert!(
                empty.contains("0 reads | mean -") && !empty.contains('\u{2588}'),
                "zero is an empty bar, not a fabricated positive count"
            );
            for row in [&long, &short, &empty] {
                assert!(
                    row.chars().count() < usize::from(width),
                    "bar and full annotation fit even at minimum width"
                );
                assert!(!row.contains('='), "ASCII bars never return");
            }
            let scrolled = frame(&regions, &snapshot, 1, width, 11);
            assert_eq!(
                text(scrolled.get(6).context("scrolled short")?),
                short,
                "offscreen maximum still determines the bar scale"
            );
            assert_eq!(
                scrolled.get(4),
                lines.get(4),
                "ruler stays identical on scroll"
            );
            assert_eq!(
                lines
                    .get(6)
                    .and_then(|line| line.get(1))
                    .map(|span| span.style().foreground_color),
                Some(Some(TEAL)),
                "data bars carry the series accent"
            );
        }
        Ok(())
    }
}
