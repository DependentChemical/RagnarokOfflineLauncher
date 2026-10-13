//! Step 3 - `3_Client/Ragnarok.exe`.
//!
//! The client is the "session" process: when the player closes the game we tear
//! down every service we started.

use anyhow::Result;
use std::path::Path;
use std::process::{Child, ExitStatus};
use std::time::Duration;

use crate::config::Resolved;
use crate::job::Job;
use crate::logging::Logger;
use crate::proc::{spawn_detached, Tracked};
use crate::registry::{Entry, RegistryHandle, State};

pub struct Client {
    pub child: Child,
    /// Kept so the caller can report the pid it was watching.
    pub pid: u32,
}

pub fn start_client(res: &Resolved, job: &Job, log: &Logger, reg: &RegistryHandle) -> Result<Client> {
    let exe = res.client_exe();
    let cwd = if res.client_cwd().is_dir() {
        res.client_cwd()
    } else {
        Path::new(".").to_path_buf()
    };
    let name = "Ragnarok.exe";

    // The client only opens its window when HKCU\Software\gravity\RagnarokOnline
    // points at this install. Make that true before launching, so a fresh copy
    // of the folder works without running opensetup.exe by hand.
    if let Err(e) = crate::clientreg::ensure_client_registry(&cwd, log) {
        log.warn(format!("client registry bootstrap failed ({e:#})"));
    }

    log.step(format!("Running {name}"));
    let child = spawn_detached(&exe, &cwd, job)?;
    let tracked = Tracked::new(child.id());
    reg.upsert(
        "client",
        Entry {
            label: name.into(),
            state: State::Running,
            tracked: Some(tracked.clone()),
            order: u32::MAX / 2,
            restart: crate::config::RestartPolicy::Off,
        },
    );
    log.ok(format!("{name} Running (pid={})", tracked.pid));
    report_window(&log, &tracked);
    log.notice("Waiting for Ragnarok.exe to exit - close the game to stop the stack (or press Ctrl+C)");
    Ok(Client {
        child,
        pid: tracked.pid,
    })
}

/// Watches for the client's window so a silent failure is visible in the log
/// (a client that never opens a window usually needs its `Setup` registry
/// entry, or exits immediately).
fn report_window(log: &Logger, tracked: &Tracked) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some((hwnd, visible)) = crate::proc::window_report(tracked.pid) {
            if visible {
                log.step(format!(
                    "Ragnarok.exe window: hwnd=0x{hwnd:x} visible={visible}"
                ));
                return;
            }
            // A hidden helper window (GDI+/IME) exists but the real window has
            // not been shown yet - keep waiting.
        }
        if !tracked.is_alive() {
            log.warn("Ragnarok.exe exited before it ever showed a window");
            return;
        }
        if std::time::Instant::now() >= deadline {
            log.warn("Ragnarok.exe has not shown a window after 20s (it may still be loading)");
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Blocks until the client exits, polling so Ctrl+C is noticed promptly.
///
/// Two independent signals are used: the OS child handle, and a liveness lookup
/// keyed on (pid, start time). Either one settling ends the session - that way
/// an exited client can never be missed because of a handle problem.
pub fn wait_for_exit(client: &mut Client, reg: &RegistryHandle, log: &Logger) -> Option<ExitStatus> {
    let tracked = Tracked::new(client.pid);
    let started = std::time::Instant::now();
    let mut last_beat = started;
    loop {
        match client.child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {}
            Err(e) => {
                log.warn(format!("error waiting for Ragnarok.exe: {e}"));
                return None;
            }
        }
        if reg.is_shutting_down() {
            return None;
        }
        // Fallback: the process is gone even if the handle did not report it.
        if !tracked.is_alive() {
            std::thread::sleep(Duration::from_millis(100));
            if let Ok(Some(status)) = client.child.try_wait() {
                return Some(status);
            }
            return Some(ExitStatusExt::fake_clean());
        }
        if last_beat.elapsed() >= Duration::from_secs(60) {
            last_beat = std::time::Instant::now();
            log.step(format!(
                "Ragnarok.exe still running (pid {}, uptime {}s)",
                client.pid,
                started.elapsed().as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// `ExitStatus` cannot be constructed directly, so this carries a sentinel that
/// the caller only uses for the human-readable message.
struct ExitStatusExt;

impl ExitStatusExt {
    fn fake_clean() -> ExitStatus {
        fake_exit_status()
    }
}

#[cfg(windows)]
fn fake_exit_status() -> ExitStatus {
    use std::os::windows::process::ExitStatusExt as _;
    std::process::ExitStatus::from_raw(0)
}

#[cfg(not(windows))]
fn fake_exit_status() -> ExitStatus {
    std::process::ExitStatus::default()
}