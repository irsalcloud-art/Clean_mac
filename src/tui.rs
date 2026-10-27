//! Full-screen live view for `monitor --tui`.
//!
//! Built on the terminal itself, with no extra dependency: the alternate
//! screen and cursor control are ANSI sequences, key input uses a
//! non-canonical termios mode, the size comes from `TIOCGWINSZ`, and the
//! loop waits in poll(2) for a key or the next sample. The terminal is
//! restored on every exit path: a key, an error, a panic (the guard's
//! `Drop`), SIGTERM or SIGHUP. It only displays metrics; there is no key
//! that acts on a process.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::monitor::{Monitor, ProcessSort, Sample};
use crate::report::{human_bytes, human_duration};

static RESIZED: AtomicBool = AtomicBool::new(false);
static TERMINATE: AtomicBool = AtomicBool::new(false);

extern "C" fn on_resize(_: libc::c_int) {
    RESIZED.store(true, Ordering::Relaxed);
}

extern "C" fn on_terminate(_: libc::c_int) {
    TERMINATE.store(true, Ordering::Relaxed);
}

/// Installs `handler` for `signal` without SA_RESTART, so a blocked
/// poll(2) returns EINTR and the loop notices the flag.
fn install(signal: libc::c_int, handler: extern "C" fn(libc::c_int)) {
    // SAFETY: zeroed sigaction is valid; the handler only stores an atomic.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = handler as usize;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(signal, &action, std::ptr::null_mut());
    }
}

/// Puts the terminal into the TUI state and restores it when dropped.
struct TerminalGuard {
    saved: libc::termios,
}

impl TerminalGuard {
    fn enter() -> io::Result<TerminalGuard> {
        // SAFETY: zeroed termios is a valid out-parameter for tcgetattr.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: STDIN_FILENO is a terminal (checked by the caller).
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut saved) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut raw = saved;
        // Keys arrive one at a time and are not echoed; Ctrl-C arrives as a
        // byte so the terminal can be restored before exiting.
        raw.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;
        // SAFETY: raw is a valid termios derived from the current one.
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let guard = TerminalGuard { saved };
        // Alternate screen, hidden cursor.
        let mut out = io::stdout().lock();
        out.write_all(b"\x1b[?1049h\x1b[?25l\x1b[H\x1b[2J")?;
        out.flush()?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut out = io::stdout().lock();
        let _ = out.write_all(b"\x1b[?25h\x1b[?1049l");
        let _ = out.flush();
        // SAFETY: restores the attributes saved in enter().
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.saved) };
    }
}

/// Terminal size as (columns, rows), with a fallback when unknown.
fn terminal_size() -> (usize, usize) {
    // SAFETY: zeroed winsize is a valid out-parameter for TIOCGWINSZ.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: STDOUT_FILENO is a terminal; ws is a valid winsize.
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0;
    if ok && ws.ws_col > 0 && ws.ws_row > 0 {
        (usize::from(ws.ws_col), usize::from(ws.ws_row))
    } else {
        (80, 24)
    }
}

/// What a key press asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Sort(ProcessSort),
}

/// Maps the bytes of one read to an action. A lone Esc quits; escape
/// sequences such as arrow keys are ignored.
pub fn action_for(keys: &[u8]) -> Option<Action> {
    if keys == [0x1b] {
        return Some(Action::Quit);
    }
    if keys.first() == Some(&0x1b) {
        return None;
    }
    keys.iter().rev().find_map(|k| match k {
        b'q' | b'Q' | 0x03 => Some(Action::Quit),
        b'c' | b'C' => Some(Action::Sort(ProcessSort::Cpu)),
        b'm' | b'M' => Some(Action::Sort(ProcessSort::Memory)),
        b'f' | b'F' => Some(Action::Sort(ProcessSort::Footprint)),
        _ => None,
    })
}

fn sort_name(sort: ProcessSort) -> &'static str {
    match sort {
        ProcessSort::Cpu => "CPU",
        ProcessSort::Memory => "RSS",
        ProcessSort::Footprint => "footprint",
    }
}

/// Cuts `line` to at most `width` characters.
fn fit(line: String, width: usize) -> String {
    if line.chars().count() <= width {
        line
    } else {
        line.chars().take(width).collect()
    }
}

/// Lays out one screen: `height` lines of at most `width` characters. The
/// process list takes whatever rows remain.
pub fn render_frame(
    sample: Option<&Sample>,
    sort: ProcessSort,
    interval: Duration,
    width: usize,
    height: usize,
) -> Vec<String> {
    let mut lines = Vec::with_capacity(height);
    let footer = format!(
        "q quit  c CPU  m RSS  f footprint   sorted by {}, every {}s   (read-only: never stops processes)",
        sort_name(sort),
        interval.as_secs()
    );
    let Some(s) = sample else {
        lines.push(format!(
            "MacClean monitor — collecting the first sample ({}s)…",
            interval.as_secs()
        ));
        lines.resize(height.saturating_sub(1), String::new());
        lines.push(footer);
        return lines
            .into_iter()
            .take(height)
            .map(|l| fit(l, width))
            .collect();
    };

    let m = &s.memory;
    let pct = |part: u64, total: u64| {
        if total == 0 {
            0.0
        } else {
            part as f64 * 100.0 / total as f64
        }
    };
    lines.push(format!(
        "MacClean monitor — uptime {}, {} processes",
        human_duration(s.uptime_secs),
        s.process_count
    ));
    lines.push(format!(
        "CPU    {:5.1}%  {} cores   load {:.2} {:.2} {:.2}",
        s.cpu.usage_percent,
        s.cpu.logical_cores,
        s.cpu.load_average[0],
        s.cpu.load_average[1],
        s.cpu.load_average[2]
    ));
    lines.push(format!(
        "Memory {:5.1}%  {} used of {}, {} available",
        pct(m.used_bytes, m.total_bytes),
        human_bytes(m.used_bytes),
        human_bytes(m.total_bytes),
        human_bytes(m.available_bytes)
    ));
    lines.push(format!(
        "Swap   {} used of {}",
        human_bytes(m.swap_used_bytes),
        human_bytes(m.swap_total_bytes)
    ));
    lines.push(match &s.disk {
        Some(d) => format!(
            "Disk   {:5.1}%  {} used of {}, {} available",
            pct(d.used_bytes, d.total_bytes),
            human_bytes(d.used_bytes),
            human_bytes(d.total_bytes),
            human_bytes(d.available_bytes)
        ),
        None => "Disk   unavailable".to_string(),
    });
    lines.push(String::new());
    lines.push(format!(
        "{:>7}  {:>6}  {:>10}  {:>10}  NAME",
        "PID", "CPU%", "RSS", "FOOTPRINT"
    ));
    let rows = height.saturating_sub(lines.len() + 1);
    for p in s.top_processes.iter().take(rows) {
        lines.push(format!(
            "{:>7}  {:>6.1}  {:>10}  {:>10}  {}",
            p.pid,
            p.cpu_percent,
            human_bytes(p.resident_bytes),
            p.footprint_bytes
                .map_or_else(|| "–".to_string(), human_bytes),
            p.name
        ));
    }
    lines.resize(height.saturating_sub(1), String::new());
    lines.push(footer);
    lines
        .into_iter()
        .take(height)
        .map(|l| fit(l, width))
        .collect()
}

/// Draws a frame over the previous one without clearing the screen first,
/// so it does not flicker.
fn draw(lines: &[String]) -> io::Result<()> {
    let mut out = io::stdout().lock();
    let mut buf = String::from("\x1b[H");
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            buf.push_str("\r\n");
        }
        buf.push_str(line);
        buf.push_str("\x1b[K");
    }
    buf.push_str("\x1b[J");
    out.write_all(buf.as_bytes())?;
    out.flush()
}

/// Waits up to `timeout` for input on stdin and returns what was read.
/// Returns early, with nothing read, when a signal arrives.
fn wait_for_keys(timeout: Duration) -> io::Result<Vec<u8>> {
    let mut fd = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
    // SAFETY: fd is a valid pollfd for the duration of the call.
    let n = unsafe { libc::poll(&mut fd, 1, ms) };
    if n < 0 {
        let e = io::Error::last_os_error();
        return if e.kind() == io::ErrorKind::Interrupted {
            Ok(Vec::new())
        } else {
            Err(e)
        };
    }
    if n == 0 || fd.revents & libc::POLLIN == 0 {
        return Ok(Vec::new());
    }
    let mut buf = [0u8; 32];
    // SAFETY: buf is valid for writes of its length.
    let read = unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr().cast(), buf.len()) };
    Ok(if read > 0 {
        buf[..read as usize].to_vec()
    } else {
        Vec::new()
    })
}

/// Runs the live view until the user quits. Stdin and stdout must be
/// terminals.
pub fn run(monitor: &mut Monitor, mut sort: ProcessSort) -> io::Result<()> {
    install(libc::SIGWINCH, on_resize);
    install(libc::SIGTERM, on_terminate);
    install(libc::SIGHUP, on_terminate);
    let _guard = TerminalGuard::enter()?;

    let interval = monitor.interval();
    let mut sample: Option<Sample> = None;
    let mut next = Instant::now() + interval;
    let mut redraw = true;
    loop {
        if TERMINATE.load(Ordering::Relaxed) {
            return Ok(());
        }
        if RESIZED.swap(false, Ordering::Relaxed) {
            redraw = true;
        }
        let now = Instant::now();
        if now >= next {
            sample = Some(monitor.sample_now());
            next = now + interval;
            redraw = true;
        }
        if redraw {
            let (w, h) = terminal_size();
            draw(&render_frame(sample.as_ref(), sort, interval, w, h))?;
            redraw = false;
        }
        let keys = wait_for_keys(next.saturating_duration_since(Instant::now()))?;
        match action_for(&keys) {
            Some(Action::Quit) => return Ok(()),
            Some(Action::Sort(s)) if s != sort => {
                // Applies from the next sample; the footer shows it now.
                sort = s;
                monitor.set_sort(s);
                redraw = true;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_actions() {
        assert_eq!(action_for(b"q"), Some(Action::Quit));
        assert_eq!(action_for(&[0x03]), Some(Action::Quit));
        assert_eq!(action_for(&[0x1b]), Some(Action::Quit));
        assert_eq!(action_for(b"\x1b[A"), None);
        assert_eq!(action_for(b"f"), Some(Action::Sort(ProcessSort::Footprint)));
        assert_eq!(action_for(b"M"), Some(Action::Sort(ProcessSort::Memory)));
        assert_eq!(action_for(b"cq"), Some(Action::Quit));
        assert_eq!(action_for(b"x"), None);
        assert_eq!(action_for(b""), None);
        // No key acts on processes.
        for k in b"kKdDtT9" {
            assert_eq!(action_for(&[*k]), None);
        }
    }

    #[test]
    fn frames_fit_the_terminal_exactly() {
        let mut m = Monitor::new(Duration::from_millis(250), usize::MAX, ProcessSort::Cpu);
        let s = m.next_sample();
        for (w, h) in [(80, 24), (40, 10), (200, 60), (10, 3), (120, 1)] {
            let lines = render_frame(Some(&s), ProcessSort::Cpu, Duration::from_secs(2), w, h);
            assert_eq!(lines.len(), h, "{w}x{h}");
            assert!(lines.iter().all(|l| l.chars().count() <= w), "{w}x{h}");
        }
        let lines = render_frame(
            Some(&s),
            ProcessSort::Footprint,
            Duration::from_secs(2),
            100,
            30,
        );
        assert!(lines[0].starts_with("MacClean monitor"));
        assert!(lines.iter().any(|l| l.contains("FOOTPRINT")));
        assert!(lines.last().unwrap().contains("sorted by footprint"));
        // 7 header lines, 1 footer: the rest are processes.
        let procs = lines
            .iter()
            .skip(7)
            .take(22)
            .filter(|l| !l.is_empty())
            .count();
        assert_eq!(procs, 22.min(s.top_processes.len()));

        let waiting = render_frame(None, ProcessSort::Cpu, Duration::from_secs(2), 80, 24);
        assert_eq!(waiting.len(), 24);
        assert!(waiting[0].contains("collecting"));
    }
}
