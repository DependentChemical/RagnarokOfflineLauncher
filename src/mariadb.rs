//! Fifth console source: MariaDB's `*.err` file.
//!
//! Laragon starts MariaDB with its own console window, so its stdout is not
//! reachable from here. The error log it writes is, and it contains exactly the
//! messages you need when a database fails to start.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::logging::Logger;
use crate::registry::RegistryHandle;

/// Newest `*.err` in `dir`, if any.
fn newest_err(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("err") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(modified) = meta.modified() else { continue };
        if best.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
            best = Some((modified, path));
        }
    }
    best.map(|(_, p)| p)
}

pub fn spawn_tail(dir: PathBuf, label: String, poll_ms: u64, log: Logger, reg: RegistryHandle) {
    let _ = std::thread::Builder::new()
        .name("mariadb-tail".into())
        .spawn(move || {
            let mut open: Option<(PathBuf, BufReader<File>)> = None;
            loop {
                if reg.is_shutting_down() {
                    return;
                }
                if open.is_none() {
                    if let Some(path) = newest_err(&dir) {
                        if let Ok(mut f) = File::open(&path) {
                            // Start at the end: old entries are already known.
                            let _ = f.seek(SeekFrom::End(0));
                            log.step(format!("Tailing {label} ({})", path.display()));
                            open = Some((path, BufReader::new(f)));
                        }
                    }
                }
                if let Some((_, reader)) = open.as_mut() {
                    let mut buf = String::new();
                    match reader.read_line(&mut buf) {
                        Ok(0) => {}
                        Ok(_) => {
                            let line = crate::logging::sanitize(&buf);
                            if !line.is_empty() {
                                log.proc(&label, line);
                            }
                        }
                        Err(_) => open = None,
                    }
                }
                std::thread::sleep(Duration::from_millis(poll_ms));
            }
        });
}