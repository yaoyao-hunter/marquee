//! Terminal state: width detection, resize events, restore-on-exit.
//!
//! Everything marquee asks of the terminal lives behind the small [`Terminal`]
//! trait, so the engine and renderer can be tested against [`FakeTerminal`]
//! with no terminal involved.
//!
//! # Size detection
//!
//! * stdout **is** a terminal: ask the kernel for its real size.
//! * stdout is **not** a terminal (pipe, file, CI): there is nothing to ask,
//!   so use `$COLUMNS`/`$LINES` when they hold sane values and otherwise
//!   [`FALLBACK_COLUMNS`]×[`FALLBACK_ROWS`]. Output stays predictable instead
//!   of failing, and no escape sequence is emitted into redirected output.
//!
//! # Taking over the line, and giving it back
//!
//! [`open`] enters raw mode — so Ctrl+C arrives as a key event instead of
//! killing the process, and resizes arrive as events — and hides the cursor.
//! It never enters the alternate screen: a marquee occupies the current line
//! only. Dropping the terminal shows the cursor and leaves raw mode, and a
//! panic hook does the same before the panic message is printed, so the
//! terminal is never left damaged.

use std::io::{self, BufWriter, IsTerminal, Stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::queue;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

/// Columns assumed when the width cannot be detected (redirected stdout, CI).
pub const FALLBACK_COLUMNS: usize = 80;

/// Rows assumed when the height cannot be detected.
pub const FALLBACK_ROWS: usize = 24;

/// What the terminal reported while a marquee is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalEvent {
    /// The window changed size; the scroll must adapt to the new width.
    Resize { columns: usize, rows: usize },
    /// The user asked to stop (Ctrl+C).
    Quit,
}

/// The terminal operations marquee needs.
///
/// Deliberately small: writing bytes (it is an [`io::Write`]), knowing the
/// current size, and reporting what happened during a frame interval.
pub trait Terminal: Write {
    /// Current size in (columns, rows).
    fn size(&self) -> (usize, usize);

    /// Current width in display columns.
    fn width(&self) -> usize {
        self.size().0
    }

    /// Waits up to `timeout` and returns everything the terminal reported in
    /// that time: resizes, and Ctrl+C as [`TerminalEvent::Quit`]. An empty
    /// result means "nothing happened, draw the next frame".
    ///
    /// The wait is part of the contract: a run loop paces its frames by
    /// polling for the interval it still owes, so an implementation with
    /// nothing to report must still let that time pass.
    fn poll(&mut self, timeout: Duration) -> io::Result<Vec<TerminalEvent>>;

    /// Whether the output understands escape sequences.
    ///
    /// A terminal does; a redirected stdout does not, where cursor positioning
    /// would only put unreadable bytes into a file or a pipe. The renderer asks
    /// this before wrapping a frame.
    fn supports_escape(&self) -> bool {
        true
    }
}

/// Opens the terminal for a marquee run.
///
/// On a real terminal this enters raw mode and hides the cursor; dropping the
/// returned value restores both. When stdout is redirected there is no
/// terminal to take over, so the result writes plain text and reports no
/// events. If a real terminal refuses raw mode, marquee degrades to that same
/// plain behaviour rather than failing to scroll at all.
pub fn open() -> Box<dyn Terminal> {
    if !io::stdout().is_terminal() {
        return Box::new(PlainTerminal::new(io::stdout(), detect_size()));
    }
    match RealTerminal::enter() {
        Ok(terminal) => Box::new(terminal),
        Err(_) => Box::new(PlainTerminal::new(io::stdout(), detect_size())),
    }
}

/// Detects the terminal size, with the documented fallback for redirected
/// output.
pub fn detect_size() -> (usize, usize) {
    if io::stdout().is_terminal()
        && let Ok((columns, rows)) = crossterm::terminal::size()
        && columns > 0
        && rows > 0
    {
        return (usize::from(columns), usize::from(rows));
    }
    fallback_size(
        std::env::var("COLUMNS").ok().as_deref(),
        std::env::var("LINES").ok().as_deref(),
    )
}

/// The size to use when there is no terminal to ask: `$COLUMNS`/`$LINES` when
/// they are sane, otherwise the fallback.
fn fallback_size(columns: Option<&str>, rows: Option<&str>) -> (usize, usize) {
    (
        sane(columns).unwrap_or(FALLBACK_COLUMNS),
        sane(rows).unwrap_or(FALLBACK_ROWS),
    )
}

/// Accepts only a positive dimension; anything else means "not set usefully".
fn sane(value: Option<&str>) -> Option<usize> {
    value
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .filter(|size| *size > 0)
}

/// Translates a crossterm event into what marquee acts on. Everything else —
/// mouse input, other keys — is ignored: a marquee takes no other input.
fn translate(event: Event) -> Option<TerminalEvent> {
    match event {
        Event::Resize(columns, rows) => Some(TerminalEvent::Resize {
            columns: usize::from(columns),
            rows: usize::from(rows),
        }),
        Event::Key(key) if is_quit(key) => Some(TerminalEvent::Quit),
        _ => None,
    }
}

/// Ctrl+C is the only way to ask a marquee to stop.
fn is_quit(key: KeyEvent) -> bool {
    key.kind != KeyEventKind::Release
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c' | 'C'))
}

/// Whether we currently hold terminal state that has to be given back.
static RAW_MODE: AtomicBool = AtomicBool::new(false);
static CURSOR_HIDDEN: AtomicBool = AtomicBool::new(false);
static PANIC_HOOK: AtomicBool = AtomicBool::new(false);

/// Gives the terminal back: cursor visible, raw mode off. Idempotent, and
/// callable from a panic hook that has no handle on the terminal.
fn restore() {
    if CURSOR_HIDDEN.swap(false, Ordering::AcqRel) {
        let _ = exit_sequence(&mut io::stdout());
    }
    if RAW_MODE.swap(false, Ordering::AcqRel) {
        let _ = disable_raw_mode();
    }
}

/// The escape sequences that take over the current line — and only that line.
fn enter_sequence(out: &mut impl Write) -> io::Result<()> {
    // No EnterAlternateScreen here, on purpose: a marquee scrolls on the line
    // the shell left it on, and the scrollback stays intact.
    queue!(out, Hide)?;
    out.flush()
}

/// The escape sequences that give the line back.
fn exit_sequence(out: &mut impl Write) -> io::Result<()> {
    queue!(out, Show)?;
    out.flush()
}

/// Restores the terminal before a panic message is printed, whatever the
/// panic strategy (unwind or abort).
fn install_panic_hook() {
    if PANIC_HOOK.swap(true, Ordering::AcqRel) {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
}

/// A real terminal: raw mode, hidden cursor, live resize and Ctrl+C events.
///
/// Dropping it restores the terminal.
pub struct RealTerminal {
    out: BufWriter<Stdout>,
}

impl RealTerminal {
    /// Takes over the current line. The caller must drop the result (or panic)
    /// for the terminal to be restored.
    pub fn enter() -> io::Result<Self> {
        install_panic_hook();
        enable_raw_mode()?;
        RAW_MODE.store(true, Ordering::Release);
        enter_sequence(&mut io::stdout())?;
        CURSOR_HIDDEN.store(true, Ordering::Release);
        Ok(Self {
            out: BufWriter::new(io::stdout()),
        })
    }
}

impl Write for RealTerminal {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.out.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

impl Terminal for RealTerminal {
    fn size(&self) -> (usize, usize) {
        detect_size()
    }

    fn poll(&mut self, timeout: Duration) -> io::Result<Vec<TerminalEvent>> {
        let mut events = Vec::new();
        let mut waited = false;
        loop {
            // Block only until the first event; drain whatever else is
            // already queued without waiting, so pacing stays accurate.
            if !event::poll(if waited { Duration::ZERO } else { timeout })? {
                break;
            }
            waited = true;
            if let Some(translated) = translate(event::read()?) {
                let quit = translated == TerminalEvent::Quit;
                events.push(translated);
                if quit {
                    break;
                }
            }
        }
        Ok(events)
    }
}

impl Drop for RealTerminal {
    fn drop(&mut self) {
        // Whatever is still buffered belongs on screen before the cursor
        // reappears.
        let _ = self.out.flush();
        restore();
    }
}

/// Redirected output: plain bytes, fixed size, no events, no escape sequences.
/// Nothing is emitted that a file or a pipe should not contain — which is also
/// why a frame is a line here rather than a cursor dance.
pub struct PlainTerminal<W> {
    out: W,
    size: (usize, usize),
}

impl<W: Write> PlainTerminal<W> {
    /// Plain output to `out`, sized by [`detect_size`].
    pub fn new(out: W, size: (usize, usize)) -> Self {
        Self { out, size }
    }
}

#[cfg(test)]
impl<W> PlainTerminal<W> {
    /// The writer, back again — what a redirected run actually produced.
    pub fn into_inner(self) -> W {
        self.out
    }
}

impl<W: Write> Write for PlainTerminal<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.out.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

impl<W: Write> Terminal for PlainTerminal<W> {
    fn size(&self) -> (usize, usize) {
        self.size
    }

    fn poll(&mut self, timeout: Duration) -> io::Result<Vec<TerminalEvent>> {
        // No terminal, so no events — but the frame interval still has to run
        // down, or a redirected run would draw as fast as it can write.
        std::thread::sleep(timeout);
        Ok(Vec::new())
    }

    fn supports_escape(&self) -> bool {
        false
    }
}

#[cfg(test)]
/// A terminal that is not there: scripted size and events, recorded output.
///
/// This is what the engine and renderer tests use instead of a real terminal.
pub struct FakeTerminal {
    size: (usize, usize),
    events: Vec<TerminalEvent>,
    scheduled: Vec<(usize, TerminalEvent)>,
    sizes: Vec<(usize, usize, usize)>,
    polls: usize,
    written: Vec<u8>,
    frames: Vec<String>,
    flushes: usize,
    frame_start: usize,
}

#[cfg(test)]
impl FakeTerminal {
    /// A fake terminal `columns` wide that has nothing to report.
    pub fn new(columns: usize, rows: usize) -> Self {
        Self {
            size: (columns, rows),
            events: Vec::new(),
            scheduled: Vec::new(),
            sizes: Vec::new(),
            polls: 0,
            written: Vec::new(),
            frames: Vec::new(),
            flushes: 0,
            frame_start: 0,
        }
    }

    /// Resizes the fake terminal, as a window resize would.
    pub fn set_size(&mut self, columns: usize, rows: usize) {
        self.size = (columns, rows);
    }

    /// Queues an event for the next [`Terminal::poll`].
    pub fn push_event(&mut self, event: TerminalEvent) {
        self.events.push(event);
    }

    /// Queues an event for the `poll`th call to [`Terminal::poll`], counted
    /// from 1 — how a run loop is tested against something that happens in the
    /// middle of a scroll: a window resized after three frames, a Ctrl+C after
    /// ten.
    pub fn push_event_at(&mut self, poll: usize, event: TerminalEvent) {
        self.scheduled.push((poll, event));
    }

    /// Resizes the fake terminal on the `poll`th call to [`Terminal::poll`],
    /// reporting it as a window resize would: the new size is what `size()`
    /// answers from then on, and the event is delivered on the same poll.
    pub fn push_resize_at(&mut self, poll: usize, columns: usize, rows: usize) {
        self.sizes.push((poll, columns, rows));
    }

    /// How many times the run loop has polled.
    pub fn poll_count(&self) -> usize {
        self.polls
    }

    /// Everything written so far, as text.
    pub fn written(&self) -> String {
        String::from_utf8_lossy(&self.written).into_owned()
    }

    /// The bytes written between each flush, as text: one entry per frame.
    pub fn frames(&self) -> &[String] {
        &self.frames
    }

    /// How many times the renderer flushed.
    pub fn flush_count(&self) -> usize {
        self.flushes
    }
}

#[cfg(test)]
impl Write for FakeTerminal {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        let pending = String::from_utf8_lossy(&self.written[self.frame_start..]).into_owned();
        self.frame_start = self.written.len();
        self.frames.push(pending);
        Ok(())
    }
}

#[cfg(test)]
impl Terminal for FakeTerminal {
    fn size(&self) -> (usize, usize) {
        self.size
    }

    fn poll(&mut self, _timeout: Duration) -> io::Result<Vec<TerminalEvent>> {
        self.polls += 1;
        let poll = self.polls;
        let mut events = std::mem::take(&mut self.events);

        let mut later = Vec::new();
        for (due, event) in self.scheduled.drain(..) {
            if due == poll {
                events.push(event);
            } else {
                later.push((due, event));
            }
        }
        self.scheduled = later;

        let mut sizes = Vec::new();
        for (due, columns, rows) in self.sizes.drain(..) {
            if due == poll {
                self.size = (columns, rows);
                events.push(TerminalEvent::Resize { columns, rows });
            } else {
                sizes.push((due, columns, rows));
            }
        }
        self.sizes = sizes;

        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{Event as CtEvent, MouseButton, MouseEvent, MouseEventKind};

    #[test]
    fn fallback_size_prefers_the_environment_and_rejects_rubbish() {
        assert_eq!(fallback_size(None, None), (FALLBACK_COLUMNS, FALLBACK_ROWS));
        assert_eq!(fallback_size(Some("120"), Some("30")), (120, 30));
        assert_eq!(fallback_size(Some(" 70 "), Some("10")), (70, 10));
        // Unset, empty, zero, negative and non-numeric all mean "not useful".
        for bad in [Some(""), Some("0"), Some("-5"), Some("wide"), None] {
            assert_eq!(
                fallback_size(bad, None).0,
                FALLBACK_COLUMNS,
                "COLUMNS={bad:?}"
            );
            assert_eq!(fallback_size(None, bad).1, FALLBACK_ROWS, "LINES={bad:?}");
        }
    }

    #[test]
    fn detected_size_is_never_zero() {
        // Under `cargo test` stdout is captured, i.e. not a terminal: this
        // exercises the documented fallback path.
        let (columns, rows) = detect_size();
        assert!(columns > 0 && rows > 0, "got {columns}x{rows}");
    }

    #[test]
    fn only_resizes_and_ctrl_c_are_reported() {
        assert_eq!(
            translate(CtEvent::Resize(70, 24)),
            Some(TerminalEvent::Resize {
                columns: 70,
                rows: 24
            })
        );

        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(translate(CtEvent::Key(ctrl_c)), Some(TerminalEvent::Quit));
        let ctrl_shift_c = KeyEvent::new(
            KeyCode::Char('C'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(
            translate(CtEvent::Key(ctrl_shift_c)),
            Some(TerminalEvent::Quit)
        );

        // A key release is not a second quit, and nothing else is an event.
        let release = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Release,
            state: crossterm::event::KeyEventState::empty(),
        };
        assert_eq!(translate(CtEvent::Key(release)), None);
        let plain_key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(translate(CtEvent::Key(plain_key)), None);
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(translate(CtEvent::Mouse(mouse)), None);
    }

    #[test]
    fn taking_the_line_never_enters_the_alternate_screen() {
        let mut enter = Vec::new();
        enter_sequence(&mut enter).expect("writing to a Vec cannot fail");
        let mut exit = Vec::new();
        exit_sequence(&mut exit).expect("writing to a Vec cannot fail");

        let entered = String::from_utf8_lossy(&enter);
        let exited = String::from_utf8_lossy(&exit);
        assert_eq!(entered, "\x1b[?25l", "hide the cursor and nothing else");
        assert_eq!(exited, "\x1b[?25h", "show the cursor again");
        // The alternate screen would swallow the shell's scrollback.
        for sequence in ["\x1b[?1049h", "\x1b[?47h", "\x1b[?1047h"] {
            assert!(!entered.contains(sequence), "entered the alternate screen");
            assert!(!exited.contains(sequence), "left the alternate screen");
        }
    }

    #[test]
    fn a_resize_is_observed_while_running_not_only_at_startup() {
        let mut terminal = FakeTerminal::new(120, 24);
        assert_eq!(terminal.width(), 120);

        // The window shrinks mid-run: the next poll reports it and the size
        // a renderer reads has changed.
        terminal.push_event(TerminalEvent::Resize {
            columns: 70,
            rows: 24,
        });
        terminal.set_size(70, 24);
        assert_eq!(
            terminal.poll(Duration::ZERO).expect("a fake never fails"),
            vec![TerminalEvent::Resize {
                columns: 70,
                rows: 24
            }]
        );
        assert_eq!(terminal.width(), 70);

        // Nothing left to report once drained.
        assert!(
            terminal
                .poll(Duration::ZERO)
                .expect("a fake never fails")
                .is_empty()
        );
    }

    #[test]
    fn plain_terminals_write_bytes_and_report_no_events() {
        let mut terminal = PlainTerminal::new(Vec::new(), (64, 12));
        assert_eq!(terminal.size(), (64, 12));
        assert_eq!(terminal.width(), 64);
        assert!(
            terminal
                .poll(Duration::ZERO)
                .expect("plain never fails")
                .is_empty()
        );

        terminal
            .write_all("你好\r\n".as_bytes())
            .expect("writing to a Vec cannot fail");
        terminal.flush().expect("flushing a Vec cannot fail");
        let PlainTerminal { out, .. } = terminal;
        assert_eq!(out, "你好\r\n".as_bytes());
    }

    #[test]
    fn the_fake_terminal_records_one_frame_per_flush() {
        let mut terminal = FakeTerminal::new(20, 3);
        write!(terminal, "你好").expect("a fake never fails");
        terminal.flush().expect("a fake never fails");
        write!(terminal, "  ").expect("a fake never fails");
        terminal.flush().expect("a fake never fails");

        assert_eq!(terminal.frames(), ["你好", "  "]);
        assert_eq!(terminal.flush_count(), 2);
        assert_eq!(terminal.written(), "你好  ");
    }
}
