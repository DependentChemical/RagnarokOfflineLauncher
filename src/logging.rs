use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// `Running laragon.exe`
    Step,
    /// `laragon.exe Running`
    Ok,
    /// `[login-server.exe] ...`
    Proc,
    /// serv.bat notices: `Login-Server has crashed! ...`
    Notice,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub level: Level,
    /// Source label for `Proc` lines, e.g. `login-server.exe`.
    pub source: Option<String>,
    pub text: String,
}

/// Single owner of stdout (and the optional log file), so every line from every
/// process is serialized into exactly one console.
pub struct Logger {
    tx: Sender<Msg>,
    ack_tx: Sender<()>,
    /// Only the original `Logger` (never its clones) keeps this.
    ack_rx: std::sync::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    colors: bool,
    timestamps: bool,
}

enum Msg {
    Line(LogLine),
    /// Stop after everything queued so far has been written.
    Quit,
}

impl Logger {
    pub fn new(colors: bool, timestamps: bool, log_file: Option<PathBuf>) -> Logger {
        let (tx, rx) = channel::<Msg>();
        let (ack_tx, ack_rx) = channel::<()>();
        let ack_writer = ack_tx.clone();
        let file = log_file.as_ref().and_then(|p| {
            if p.exists() {
                OpenOptions::new().append(true).open(p).ok()
            } else {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)
                    .ok()
            }
        });
        let _handle = std::thread::Builder::new()
            .name("logger".into())
            .spawn(move || {
                let mut file = file.map(BufWriter::new);
                // The stdout lock is taken per line, never held for the thread's
                // lifetime: holding it permanently would deadlock every other
                // writer in the process (including `println!`).
                while let Ok(msg) = rx.recv() {
                    let line = match msg {
                        Msg::Quit => break,
                        Msg::Line(l) => l,
                    };
                    let plain = render(&line, false, timestamps);
                    let painted = render(&line, colors, timestamps);
                    {
                        let stdout = std::io::stdout();
                        let mut out = stdout.lock();
                        let _ = writeln!(out, "{painted}");
                        let _ = out.flush();
                    }
                    if let Some(f) = file.as_mut() {
                        let _ = writeln!(f, "{plain}");
                        let _ = f.flush();
                    }
                }
                // Tell `finish()` that everything queued has been written.
                let _ = ack_writer.send(());
            })
            .ok();
        Logger {
            tx,
            ack_tx,
            ack_rx: std::sync::Mutex::new(Some(ack_rx)),
            colors,
            timestamps,
        }
    }

    /// Drains the queue and waits for the writer thread to finish.
    ///
    /// Must be called before `std::process::exit`, otherwise the last lines
    /// (the shutdown summary, typically) are lost with the thread.
    pub fn finish(&self) {
        let _ = self.tx.send(Msg::Quit);
        let wait = self.ack_rx.lock().ok().and_then(|mut g| g.take());
        if let Some(rx) = wait {
            let _ = rx.recv_timeout(std::time::Duration::from_secs(3));
        }
    }

    fn send(&self, level: Level, source: Option<&str>, text: impl Into<String>) {
        // A closed channel just means the writer thread died; never panic.
        let _ = self.tx.send(Msg::Line(LogLine {
            level,
            source: source.map(|s| s.to_string()),
            text: text.into(),
        }));
    }

    pub fn step(&self, msg: impl Into<String>) {
        self.send(Level::Step, None, msg);
    }

    pub fn ok(&self, msg: impl Into<String>) {
        self.send(Level::Ok, None, msg);
    }

    pub fn notice(&self, msg: impl Into<String>) {
        self.send(Level::Notice, None, msg);
    }

    pub fn warn(&self, msg: impl Into<String>) {
        self.send(Level::Warn, None, msg);
    }

    pub fn error(&self, msg: impl Into<String>) {
        self.send(Level::Error, None, msg);
    }

    /// Console output captured from a child process, prefixed with its name.
    pub fn proc(&self, source: &str, text: impl Into<String>) {
        self.send(Level::Proc, Some(source), text);
    }
}

impl Clone for Logger {
    fn clone(&self) -> Self {
        // Clones share the writer thread; they do not own the ack channel.
        Logger {
            tx: self.tx.clone(),
            ack_tx: self.ack_tx.clone(),
            ack_rx: std::sync::Mutex::new(None),
            colors: self.colors,
            timestamps: self.timestamps,
        }
    }
}

fn _unused() {}

fn timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let ms = now.subsec_millis();
    // HH:MM:SS.mmm without pulling in a date/time dependency.
    let day_secs = secs % 86_400;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        day_secs / 3600,
        (day_secs % 3600) / 60,
        day_secs % 60,
        ms
    )
}

fn color_for_source(src: &str) -> &'static str {
    // Stable per-process color so the eye can track a server in the noise.
    const COLORS: [&str; 6] = ["\x1b[36m", "\x1b[32m", "\x1b[33m", "\x1b[35m", "\x1b[34m", "\x1b[95m"];
    let mut h: usize = 0;
    for b in src.bytes() {
        h = h.wrapping_mul(31).wrapping_add(b as usize);
    }
    COLORS[h % COLORS.len()]
}

fn render(line: &LogLine, colors: bool, timestamps: bool) -> String {
    let paint = |code: &str, s: &str| -> String {
        if colors && !code.is_empty() {
            format!("{code}{s}\x1b[0m")
        } else {
            s.to_string()
        }
    };
    let ts = if timestamps {
        format!("{} ", timestamp())
    } else {
        String::new()
    };
    match (&line.level, &line.source) {
        (Level::Proc, Some(src)) => format!("{ts}{}[{src}] {}", color_for_source(src), line.text),
        (Level::Step, _) => format!("{ts}{}", paint("\x1b[1;37m", &line.text)),
        (Level::Ok, _) => format!("{ts}{}", paint("\x1b[1;32m", &line.text)),
        (Level::Notice, _) => format!("{ts}{}", paint("\x1b[1;33m", &line.text)),
        (Level::Warn, _) => format!("{ts}{}", paint("\x1b[1;31m", &line.text)),
        (Level::Error, _) => format!("{ts}{}", paint("\x1b[1;41;97m", &line.text)),
        _ => format!("{ts}{}", line.text),
    }
}

/// Removes ANSI escape sequences and normalizes a raw console line.
pub fn sanitize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI ... final byte in 0x40..=0x7E
            if chars.peek() == Some(&'[') {
                chars.next();
                for c2 in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c2) {
                        break;
                    }
                }
            }
            continue;
        }
match c {
            '\r' => continue,
            '\n' => continue,
            _ => out.push(c),
        }
    }
    out.trim_end().to_string()
}