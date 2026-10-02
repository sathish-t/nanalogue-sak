//! Crossterm screen lifecycle, polling and keyboard navigation.

use std::io::{self, Write as _};
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
        execute!(io::stdout(), EnterAlternateScreen, Hide)?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _restore = execute!(
            io::stdout(),
            EndSynchronizedUpdate,
            ResetColor,
            Show,
            LeaveAlternateScreen
        );
        let _raw_mode = terminal::disable_raw_mode();
    }
}

/// Draws the current snapshot and returns the number of visible region rows.
fn redraw(regions: &[Region], snapshot: &MonitorSnapshot, offset: &mut usize) -> Result<usize> {
    let (width, height) = terminal::size()?;
    let visible = usize::from(height).saturating_sub(CHROME_ROWS).max(1);
    *offset = (*offset).min(regions.len().saturating_sub(visible));
    draw(&frame(regions, snapshot, *offset, width, height))?;
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
    let mut offset: usize = 0;
    let mut next_poll = Instant::now();
    loop {
        let visible = redraw(regions, monitor.snapshot(), &mut offset)?;

        let now = Instant::now();
        if now < next_poll && event::poll(next_poll.saturating_duration_since(now))? {
            if handle_event(event::read()?, &mut offset, visible, regions.len()).is_break() {
                break;
            }
            continue;
        }

        let outcome = monitor.refresh(|snapshot| {
            let progress_visible = redraw(regions, snapshot, &mut offset)?;
            if event::poll(Duration::ZERO)? {
                Ok(handle_event(
                    event::read()?,
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
        next_poll = Instant::now()
            .checked_add(POLL_INTERVAL)
            .unwrap_or_else(Instant::now);
    }
    Ok(())
}

/// Writes complete, cleared lines without letting terminal autowrap scroll.
fn draw(lines: &[Line]) -> Result<()> {
    let mut output = io::stdout().lock();
    let columns = usize::from(terminal::size()?.0).saturating_sub(1);
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
mod tests {
    //! Keyboard behavior shared by waiting and scanning states.
    use super::*;

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
        let outcome = handle_event(
            Event::Key(event::KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)),
            &mut offset,
            3,
            10,
        );
        assert!(outcome.is_continue(), "navigation continues monitoring");
        assert_eq!(offset, 5, "page navigation uses the visible row count");
    }
}
