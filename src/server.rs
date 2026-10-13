//! Step 2 - the logic of `2_Server/runserver.bat`, reimplemented in Rust.
//!
//! runserver.bat "watch":
//!   for each server -> :getXStatus (tasklist by image name)
//!                     -> start cmd /k <x>serv.bat on
//!   serv.bat runs the exe, then loops forever:
//!     exit 0    -> "Label has shutdown successfully."
//!     exit != 0 -> "Label has terminated abnormally."
//!     restart_mode=on -> "Restarting in 15 seconds, press Ctrl+C to cancel."
//!
//! Everything except the batch plumbing is preserved here; the only difference
//! is that output is captured instead of shown in separate console windows.

use anyhow::Result;
use std::path::PathBuf;
use std::process::{Child, ExitStatus};
use std::thread;
use std::time::Duration;

use crate::config::{Resolved, ServerSpec};
use crate::gate::wait_for_port;
use crate::job::Job;
use crate::logging::Logger;
use crate::proc::{find_by_name, spawn_captured, TermResult, Tracked};
use crate::registry::{Entry, RegistryHandle, State};

pub fn start_all_servers(
    res: &Resolved,
    job: &Job,
    log: &Logger,
    reg: &RegistryHandle,
) -> Result<()> {
    log.step("Starting all serv");
    for (order, spec) in res.cfg.servers.iter().enumerate() {
        let order = order as u32 + 1;
        let exe = res.server_exe(spec);

        // :getXStatus - skip if an instance is already alive (and not ours).
        if let Some((pid, _)) = find_by_name(&spec.exe) {
            log.notice(format!(
                "{} serv is already running, pid={}",
                spec.label, pid
            ));
            reg.upsert(
                &spec.key,
                Entry {
                    label: spec.exe.clone(),
                    state: State::Running,
                    tracked: None,
                    order,
                    restart: spec.restart,
                },
            );
            continue;
        }

        spawn_and_supervise(res, job, log, reg, spec, exe, order);
        // Strictly sequential boot: the next server (and the client) wait for
        // this one to be listening, exactly like watching `runserver.bat watch`
        // open each console in turn.
        if spec.ready_port.is_some() {
            reg.wait_for_running(&spec.key, Duration::from_secs(res.cfg.server_ready_timeout_secs));
        }
    }
    Ok(())
}

fn spawn_and_supervise(
    res: &Resolved,
    job: &Job,
    log: &Logger,
    reg: &RegistryHandle,
    spec: &ServerSpec,
    exe: PathBuf,
    order: u32,
) {
    let job = job.clone();
    let log = log.clone();
    let reg = reg.clone();
    let spec = spec.clone();
    let cwd = res.server_cwd(&spec);
    let delay = res.cfg.restart_delay_secs;
    let ready_timeout = res.cfg.server_ready_timeout_secs;

    let _ = thread::Builder::new()
        .name(format!("sup-{}", spec.key))
        .spawn(move || {
            supervise(log, reg, job, spec, exe, cwd, order, delay, ready_timeout);
        });
}

/// The `serv.bat` state machine, one thread per server.
#[allow(clippy::too_many_arguments)]
fn supervise(
    log: Logger,
    reg: RegistryHandle,
    job: Job,
    spec: ServerSpec,
    exe: PathBuf,
    cwd: PathBuf,
    order: u32,
    delay: u64,
    ready_timeout: u64,
) {
    let key = spec.key.clone();
    reg.upsert(
        &key,
        Entry {
            label: spec.exe.clone(),
            state: State::Starting,
            tracked: None,
            order,
            restart: spec.restart,
        },
    );

    loop {
        if reg.is_shutting_down() {
            return;
        }
        log.step(format!("Running {}", spec.exe));
        reg.set_state(&key, State::Starting);

        let (mut child, tracked): (Child, Tracked) =
            match spawn_captured(&exe, &cwd, &job, &log, &spec.exe) {
                Ok(v) => v,
                Err(e) => {
                    log.error(format!("{}: failed to start ({e:#})", spec.label));
                    reg.set_state(&key, State::Exited(-1));
                    if !reg.interruptible_sleep(delay, Some(&key)) {
                        return;
                    }
                    continue;
                }
            };

        reg.set_tracked(&key, Some(tracked.clone()));
        reg.set_state(&key, State::Running);

        match spec.ready_port {
            Some(port) => {
                if wait_for_port(&log, "127.0.0.1", port, ready_timeout, &spec.exe) {
                    log.ok(format!("{} Running (pid={})", spec.exe, tracked.pid));
                } else {
                    log.warn(format!(
                        "{}.exe started (pid={}) but port {} never opened",
                        spec.exe, tracked.pid, port
                    ));
                }
            }
            None => log.ok(format!("{} Running (pid={})", spec.exe, tracked.pid)),
        }

        let Some(status) = wait_smartly(&mut child, &tracked, &reg) else {
            // We killed it during shutdown.
            reg.set_state(&key, State::Exited(0));
            return;
        };

        announce_exit(&log, &spec, status);
        reg.set_state(&key, State::Exited(status.code().unwrap_or(-1)));

        if !spec.restart.as_bool() || reg.is_shutting_down() {
            return;
        }
        // runserver.bat "watch": restart forever after 15 seconds.
        reg.set_state(&key, State::WaitingRestart(delay));
        log.notice(format!(
            "{} Restarting in {} seconds, press Ctrl+C to cancel.",
            spec.label, delay
        ));
        if !reg.interruptible_sleep(delay, Some(&key)) {
            return;
        }
        // :getXStatus runs again before every restart; never double-spawn.
        if let Some((pid, _)) = find_by_name(&spec.exe) {
            log.notice(format!(
                "{} serv is already running, pid={}",
                spec.label, pid
            ));
        }
    }
}

/// `wait()` that also notices when *we* killed the child during shutdown.
fn wait_smartly(child: &mut Child, tracked: &Tracked, reg: &RegistryHandle) -> Option<ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if reg.is_shutting_down() {
                    return None;
                }
                // If it died on its own between polls, try_wait picks it up.
                if !tracked.is_alive() {
                    thread::sleep(Duration::from_millis(50));
                    return child.try_wait().ok().flatten();
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return None,
        }
    }
}

/// serv.bat's exit vocabulary.
fn announce_exit(log: &Logger, spec: &ServerSpec, status: ExitStatus) {
    match status.code() {
        Some(0) => log.notice(format!("{} has shutdown successfully.", spec.label)),
        Some(code) => log.error(format!(
            "{} has terminated abnormally (exit code {}).",
            spec.label, code
        )),
        None => log.error(format!("{} has crashed! (killed)", spec.label)),
    }
}

/// Interactive `r <key>`: stop the server; its supervisor restarts it at once.
pub fn stop_server(key: &str, reg: &RegistryHandle, log: &Logger) {
    let Some((_, entry)) = reg.snapshot().into_iter().find(|(k, _)| k == key) else {
        log.warn(format!("no managed process with key '{key}'"));
        return;
    };
    let Some(t) = entry.tracked.clone() else {
        log.notice(format!("{} is not managed by this launcher", entry.label));
        return;
    };
    reg.set_state(key, State::Stopping);
    match t.terminate() {
        TermResult::Terminated => log.step(format!("Stopping {}", entry.label)),
        TermResult::AlreadyGone => log.notice(format!("{} already stopped", entry.label)),
        TermResult::Recycled => log.warn(format!(
            "{} (pid {}) already exited and the pid was reused - not killing it",
            entry.label, t.pid
        )),
        TermResult::Failed(e) => log.error(format!("{}: {e}", entry.label)),
    }
}