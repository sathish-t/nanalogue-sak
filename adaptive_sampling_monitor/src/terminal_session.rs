//! Crossterm screen lifecycle, polling and keyboard navigation.

use std::io;
use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::{PrintStyledContent, ResetColor};
use crossterm::terminal::{
    self, BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use crossterm::{execute, queue};

use crate::bed::Region;
use crate::error::Result;
use crate::monitor::{Monitor, MonitorSnapshot};
use crate::terminal_frame::{CHROME_ROWS, Line, fit, frame};

/// Delay between synchronous directory scans.
const POLL_INTERVAL: Duration = Duration::from_mins(1);

/// Restores terminal state on ordinary returns and stack unwinding.
#[derive(Debug)]
struct TerminalGuard;

impl TerminalGuard {
    /// Enters raw alternate-screen mode with rollback if setup fails.
    fn enter() -> Result<Self> {
        terminal::enable_raw_mode()?;
        let guard = Self;
        enter_terminal(&mut io::stdout())?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _restore = restore_terminal(&mut io::stdout());
        let _raw_mode = terminal::disable_raw_mode();
    }
}

/// Writes controls that enter the hidden-cursor alternate screen.
fn enter_terminal<W: io::Write>(output: &mut W) -> Result<()> {
    execute!(output, EnterAlternateScreen, Hide)?;
    Ok(())
}

/// Writes controls that restore the ordinary visible-cursor screen.
fn restore_terminal<W: io::Write>(output: &mut W) -> Result<()> {
    execute!(
        output,
        EndSynchronizedUpdate,
        ResetColor,
        Show,
        LeaveAlternateScreen
    )?;
    Ok(())
}

/// Draws the current snapshot and returns the number of visible region rows.
fn redraw(regions: &[Region], snapshot: &MonitorSnapshot, offset: &mut usize) -> Result<usize> {
    let (width, height) = terminal::size()?;
    let mut output = io::stdout().lock();
    redraw_to(&mut output, regions, snapshot, offset, width, height)
}

/// Draws at an explicit terminal size and output boundary.
fn redraw_to<W: io::Write>(
    output: &mut W,
    regions: &[Region],
    snapshot: &MonitorSnapshot,
    offset: &mut usize,
    width: u16,
    height: u16,
) -> Result<usize> {
    let visible = usize::from(height).saturating_sub(CHROME_ROWS).max(1);
    *offset = (*offset).min(regions.len().saturating_sub(visible));
    let columns = usize::from(width).saturating_sub(1);
    draw_to(
        output,
        &frame(regions, snapshot, *offset, width, height),
        columns,
    )?;
    Ok(visible)
}

/// Applies one terminal event identically while waiting or scanning.
#[expect(
    clippy::needless_pass_by_value,
    reason = "event::read returns an owned event that this handler consumes"
)]
fn handle_event(
    input: Event,
    offset: &mut usize,
    visible: usize,
    region_count: usize,
) -> ControlFlow<()> {
    let Event::Key(key) = input else {
        return ControlFlow::Continue(());
    };
    if key.kind == KeyEventKind::Release {
        return ControlFlow::Continue(());
    }
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "KeyCode is non-exhaustive and unhandled keys intentionally do nothing"
    )]
    match key.code {
        KeyCode::Char('q') => return ControlFlow::Break(()),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            return ControlFlow::Break(());
        }
        KeyCode::Down | KeyCode::Char('j') => *offset = offset.saturating_add(1),
        KeyCode::Up | KeyCode::Char('k') => *offset = offset.saturating_sub(1),
        KeyCode::PageDown => *offset = offset.saturating_add(visible),
        KeyCode::PageUp => *offset = offset.saturating_sub(visible),
        KeyCode::Home => *offset = 0,
        KeyCode::End => *offset = region_count.saturating_sub(visible),
        _ => {}
    }
    ControlFlow::Continue(())
}

/// Draws after input events and scans synchronously once per poll interval.
pub(crate) fn run_terminal_monitor(regions: &[Region], monitor: &mut Monitor) -> Result<()> {
    let _terminal = TerminalGuard::enter()?;
    run_monitor_loop(regions, monitor, redraw, read_event, Instant::now)
}

/// Polls once and reads the available event, if any.
fn read_event(timeout: Duration) -> Result<Option<Event>> {
    read_polled_event(timeout, event::poll, event::read)
}

/// Combines event polling and reading without blocking after an empty poll.
fn read_polled_event<Poll, Read>(
    timeout: Duration,
    mut poll: Poll,
    mut read: Read,
) -> Result<Option<Event>>
where
    Poll: FnMut(Duration) -> io::Result<bool>,
    Read: FnMut() -> io::Result<Event>,
{
    if poll(timeout)? {
        Ok(Some(read()?))
    } else {
        Ok(None)
    }
}

/// Coordinates redraws, input and scans through deterministic I/O boundaries.
fn run_monitor_loop<Redraw, ReadEvent, Now>(
    regions: &[Region],
    monitor: &mut Monitor,
    mut redraw_screen: Redraw,
    mut read_input: ReadEvent,
    mut now: Now,
) -> Result<()>
where
    Redraw: FnMut(&[Region], &MonitorSnapshot, &mut usize) -> Result<usize>,
    ReadEvent: FnMut(Duration) -> Result<Option<Event>>,
    Now: FnMut() -> Instant,
{
    let mut offset: usize = 0;
    let mut next_poll = now();
    loop {
        let visible = redraw_screen(regions, monitor.snapshot(), &mut offset)?;

        let current_time = now();
        if current_time < next_poll
            && let Some(input) = read_input(next_poll.saturating_duration_since(current_time))?
        {
            if handle_event(input, &mut offset, visible, regions.len()).is_break() {
                break;
            }
            continue;
        }

        let outcome = monitor.refresh(|snapshot| {
            let progress_visible = redraw_screen(regions, snapshot, &mut offset)?;
            if let Some(input) = read_input(Duration::ZERO)? {
                Ok(handle_event(
                    input,
                    &mut offset,
                    progress_visible,
                    regions.len(),
                ))
            } else {
                Ok(ControlFlow::Continue(()))
            }
        })?;
        if outcome.is_break() {
            break;
        }
        next_poll = now().checked_add(POLL_INTERVAL).unwrap_or_else(&mut now);
    }
    Ok(())
}

/// Writes a frame to an already selected terminal output and width.
fn draw_to<W: io::Write>(output: &mut W, lines: &[Line], columns: usize) -> Result<()> {
    queue!(output, BeginSynchronizedUpdate)?;
    for (row, line) in lines.iter().enumerate() {
        let terminal_row = u16::try_from(row)?;
        queue!(
            output,
            MoveTo(0, terminal_row),
            ResetColor,
            Clear(ClearType::CurrentLine),
        )?;
        let mut remaining = columns;
        for span in line {
            let clipped = fit(span.content(), remaining);
            remaining = remaining.saturating_sub(clipped.chars().count());
            queue!(output, PrintStyledContent(span.style().apply(clipped)))?;
        }
    }
    queue!(output, ResetColor, EndSynchronizedUpdate)?;
    output.flush()?;
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    //! Keyboard behavior shared by waiting and scanning states.
    use std::path::PathBuf;

    use super::*;

    /// Terminal setup and cleanup emit the expected alternate-screen controls.
    #[cfg(unix)]
    #[test]
    fn terminal_lifecycle_control_sequences() -> Result<()> {
        let mut output = Vec::new();
        enter_terminal(&mut output)?;
        restore_terminal(&mut output)?;
        let rendered = String::from_utf8(output)?;

        assert!(
            rendered.contains("\u{1b}[?1049h"),
            "alternate screen entered"
        );
        assert!(rendered.contains("\u{1b}[?25l"), "cursor hidden");
        assert!(rendered.contains("\u{1b}[?25h"), "cursor restored");
        assert!(rendered.contains("\u{1b}[?1049l"), "alternate screen left");
        Ok(())
    }

    /// Polling reads exactly one event only when input is reported ready.
    #[test]
    fn polled_event_reads_only_available_input() -> Result<()> {
        let timeout = Duration::from_millis(25);
        let absent = read_polled_event(
            timeout,
            |actual| {
                assert_eq!(actual, timeout, "timeout is forwarded unchanged");
                Ok(false)
            },
            || unreachable!("empty poll must not read"),
        )?;
        assert!(absent.is_none(), "empty poll returns no event");

        let expected = Event::Resize(80, 24);
        let available = read_polled_event(timeout, |_| Ok(true), || Ok(expected.clone()))?;
        assert_eq!(
            available,
            Some(expected),
            "ready input is read exactly once"
        );
        Ok(())
    }

    /// Waiting and scanning share quit and navigation event behavior.
    #[test]
    fn shared_event_handler_quits_and_navigates() {
        let mut offset = 2;
        assert!(
            handle_event(
                Event::Key(event::KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE,)),
                &mut offset,
                3,
                10,
            )
            .is_break(),
            "q quits"
        );
        assert!(
            handle_event(
                Event::Key(event::KeyEvent::new(
                    KeyCode::Char('c'),
                    KeyModifiers::CONTROL,
                )),
                &mut offset,
                3,
                10,
            )
            .is_break(),
            "Ctrl-C quits"
        );
        let page_outcome = handle_event(
            Event::Key(event::KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)),
            &mut offset,
            3,
            10,
        );
        assert!(
            page_outcome.is_continue(),
            "navigation continues monitoring"
        );
        assert_eq!(offset, 5, "page navigation uses the visible row count");

        for (code, start, expected) in [
            (KeyCode::Down, 2, 3),
            (KeyCode::Char('j'), 2, 3),
            (KeyCode::Up, 2, 1),
            (KeyCode::Char('k'), 2, 1),
            (KeyCode::PageUp, 5, 2),
            (KeyCode::Home, 5, 0),
            (KeyCode::End, 2, 7),
            (KeyCode::Char('x'), 2, 2),
            (KeyCode::Char('c'), 2, 2),
        ] {
            let mut navigation_offset = start;
            let navigation_outcome = handle_event(
                Event::Key(event::KeyEvent::new(code, KeyModifiers::NONE)),
                &mut navigation_offset,
                3,
                10,
            );
            assert!(navigation_outcome.is_continue(), "navigation does not quit");
            assert_eq!(navigation_offset, expected, "{code:?} updates the offset");
        }

        assert!(
            handle_event(Event::Resize(80, 24), &mut offset, 3, 10).is_continue(),
            "non-key events are ignored"
        );
        assert!(
            handle_event(
                Event::Key(event::KeyEvent::new_with_kind(
                    KeyCode::Down,
                    KeyModifiers::NONE,
                    KeyEventKind::Release,
                )),
                &mut offset,
                3,
                10,
            )
            .is_continue(),
            "key releases are ignored"
        );
    }

    /// Redrawing clamps scrolling and uses the exact available region-row count.
    #[cfg(unix)]
    #[test]
    fn redraw_to_clamps_offset_and_reports_visible_rows() -> Result<()> {
        let regions = (0..3)
            .map(|index| Region {
                contig: "chr1".to_owned(),
                start: index,
                end: index.saturating_add(1),
                name: format!("region-{index}"),
            })
            .collect::<Vec<_>>();
        let snapshot = MonitorSnapshot::new(regions.len());
        let mut offset = usize::MAX;
        let mut output = Vec::new();

        let visible = redraw_to(&mut output, &regions, &snapshot, &mut offset, 80, 11)?;

        assert_eq!(visible, 2, "height minus chrome determines visible rows");
        assert_eq!(offset, 1, "offset is clamped to the final full page");
        assert!(!output.is_empty(), "the complete frame is emitted");
        Ok(())
    }

    /// Complete frames are clipped and emitted with synchronized-update controls.
    #[cfg(unix)]
    #[test]
    fn draw_to_clears_and_clips_each_line() -> Result<()> {
        use crossterm::style::Stylize as _;

        let lines = vec![
            vec!["abcdef".to_owned().red()],
            vec!["xy".to_owned().bold()],
        ];
        let mut output = Vec::new();
        draw_to(&mut output, &lines, 3)?;
        let rendered = String::from_utf8(output)?;

        assert!(rendered.contains("abc"), "first line is emitted");
        assert!(!rendered.contains("def"), "first line is clipped to width");
        assert!(rendered.contains("xy"), "second line is emitted");
        assert!(rendered.contains("\u{1b}[1;1H"), "first row is selected");
        assert!(rendered.contains("\u{1b}[2;1H"), "second row is selected");
        Ok(())
    }

    /// Input can stop either an in-progress refresh or the timed waiting phase.
    #[test]
    fn monitor_loop_quits_during_refresh_and_waiting() -> Result<()> {
        let regions = Vec::new();
        let current_time = Instant::now();
        let quit = || Event::Key(event::KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));

        let mut refresh_monitor =
            Monitor::new(PathBuf::from("missing-refresh-directory"), &regions);
        let mut refresh_redraws = 0;
        run_monitor_loop(
            &regions,
            &mut refresh_monitor,
            |_, _, _| {
                refresh_redraws += 1;
                Ok(1)
            },
            |timeout| Ok(timeout.is_zero().then(quit)),
            || current_time,
        )?;
        assert_eq!(
            refresh_redraws, 2,
            "quit is observed at first refresh checkpoint"
        );

        let mut waiting_monitor = Monitor::new(PathBuf::from("missing-wait-directory"), &regions);
        let mut waiting_redraws = 0;
        run_monitor_loop(
            &regions,
            &mut waiting_monitor,
            |_, _, _| {
                waiting_redraws += 1;
                Ok(1)
            },
            |timeout| Ok((!timeout.is_zero()).then(quit)),
            || current_time,
        )?;
        assert_eq!(
            waiting_redraws, 3,
            "one refresh completes before timed waiting"
        );

        let mut navigation_monitor =
            Monitor::new(PathBuf::from("missing-navigation-directory"), &regions);
        let mut observed_offsets = Vec::new();
        let mut waiting_events = 0;
        run_monitor_loop(
            &regions,
            &mut navigation_monitor,
            |_, _, offset| {
                observed_offsets.push(*offset);
                Ok(1)
            },
            |timeout| {
                if timeout.is_zero() {
                    Ok(None)
                } else {
                    waiting_events += 1;
                    let code = if waiting_events == 1 {
                        KeyCode::Down
                    } else {
                        KeyCode::Char('q')
                    };
                    Ok(Some(Event::Key(event::KeyEvent::new(
                        code,
                        KeyModifiers::NONE,
                    ))))
                }
            },
            || current_time,
        )?;
        assert_eq!(
            observed_offsets,
            vec![0, 0, 0, 1],
            "navigation redraws at the updated offset before a later quit"
        );
        Ok(())
    }

    /// I/O boundary failures stop the loop without being hidden or retried.
    #[test]
    fn monitor_loop_propagates_redraw_and_input_errors() {
        use crate::error::message;

        let regions = Vec::new();
        let current_time = Instant::now();
        let mut redraw_monitor = Monitor::new(PathBuf::from("unused-redraw-directory"), &regions);
        let redraw_error = run_monitor_loop(
            &regions,
            &mut redraw_monitor,
            |_, _, _| Err(message("redraw failed")),
            |_| -> Result<Option<Event>> { unreachable!("input follows a successful redraw") },
            || current_time,
        )
        .expect_err("redraw failure must stop the loop");
        assert_eq!(redraw_error.to_string(), "redraw failed");

        let mut input_monitor = Monitor::new(PathBuf::from("unused-input-directory"), &regions);
        let input_error = run_monitor_loop(
            &regions,
            &mut input_monitor,
            |_, _, _| Ok(1),
            |_| Err(message("input failed")),
            || current_time,
        )
        .expect_err("input failure must stop the loop");
        assert_eq!(input_error.to_string(), "input failed");
    }
}
