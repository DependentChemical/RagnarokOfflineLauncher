//! Plain-console control channel.
//!
//! Since the UI is a plain console (no alternate screen), a single stdin reader
//! gives us the essentials without a TUI:
//!   q / quit  -> graceful shutdown (same path as closing the client)
//!   s / status-> print the tracked process table
//!   r <key>   -> restart one server (skips its 15s crash delay)

use std::io::BufRead;

use crate::logging::Logger;
use crate::registry::{RegistryHandle, State};
use crate::server::stop_server;

pub fn spawn(log: Logger, reg: RegistryHandle) {
    let _ = std::thread::Builder::new()
        .name("console".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { return };
                let cmd = line.trim().to_ascii_lowercase();
                if cmd.is_empty() {
                    continue;
                }
                match cmd.as_str() {
                    "q" | "quit" | "exit" => {
                        log.notice("Shutdown requested from console.");
                        reg.begin_shutdown();
                        return;
                    }
                    "s" | "status" => print_status(&reg),
                    "h" | "help" => {
                        log.notice("commands: s = status, r <login|char|web|map> = restart, q = quit");
                    }
                    other => {
                        if let Some(key) = other.strip_prefix("r ") {
                            let key = key.trim();
                            stop_server(key, &reg, &log);
                            reg.request_restart(key);
                        } else {
                            log.warn(format!("unknown command '{cmd}' (try h)"));
                        }
                    }
                }
            }
        });
}

pub fn print_status(reg: &RegistryHandle) {
    let rows = reg.snapshot();
    let width = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(6).max(6);
    eprintln!();
    eprintln!(
        "{:<width$}  {:<22}  {:<12}  {:<9}  {}",
        "KEY", "PROCESS", "STATE", "PID", "RESTART",
        width = width
    );
    eprintln!("{}", "-".repeat(width + 66));
    for (key, entry) in rows {
        let pid = entry
            .tracked
            .as_ref()
            .map(|t| t.pid.to_string())
            .unwrap_or_else(|| "-".into());
        let state = match &entry.state {
            State::Exited(c) if *c == 0 && reg.is_shutting_down() => "stopped".into(),
            s => s.text(),
        };
        eprintln!(
            "{:<width$}  {:<22}  {:<12}  {:<9}  {}",
            key,
            entry.label,
            state,
            pid,
            if entry.restart.as_bool() { "on" } else { "off" },
            width = width
        );
    }
    eprintln!();
}