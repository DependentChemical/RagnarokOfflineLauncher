//! ro_launcher - one console for the whole RO_PreRenewal stack.
//!
//! Creates a space-free junction (e.g. D:\RO_OFFLINE) pointing at the project root,
//! then launches all programs directly under that junction. No config rewriting needed.

mod client;
mod clientreg;
mod config;
mod console;
mod database;
mod elevate;
mod gate;
mod job;
mod logging;
mod proc;
mod registry;
mod server;
mod symlink;

use anyhow::{bail, Result};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use config::Resolved;
use job::Job;
use logging::Logger;
use proc::TermResult;
use registry::{Registry, RegistryHandle, State};
use symlink::{acquire_lock, cleanup_symlink, ensure_symlink, release_lock};

struct Args {
    root: Option<PathBuf>,
    dry_run: bool,
    no_elevate: bool,
    help: bool,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        root: None,
        dry_run: false,
        no_elevate: false,
        help: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--root" => {
                a.root = Some(PathBuf::from(
                    it.next().ok_or_else(|| anyhow::anyhow!("--root needs a path"))?,
                ))
            }
            "--dry-run" | "--print-config" => a.dry_run = true,
            "--no-elevate" => a.no_elevate = true,
            "-h" | "--help" => a.help = true,
            other => bail!("unknown argument '{other}' (try --help)"),
        }
    }
    Ok(a)
}

const HELP: &str = r#"ro_launcher - consolidated launcher for the RO_PreRenewal stack

USAGE:
    ro_launcher [OPTIONS]

OPTIONS:
    --root <DIR>     Project root (default: auto-detected next to the exe,
                     or RO_ROOT, or the nearest ancestor holding 1_Database)
    --dry-run        Validate configuration and print the boot plan, spawn nothing
    --no-elevate     Never re-launch through UAC (Ragnarok.exe requires it)
    -h, --help       This text

CONSOLE COMMANDS (while running):
    s            status table
    r <key>      restart a server (login | char | web | map)
    q            shutdown everything (same as closing Ragnarok.exe)
    Ctrl+C       shutdown everything
"#;

static LOG_HANDLE: std::sync::OnceLock<std::sync::Mutex<Option<std::sync::Arc<Logger>>>> =
    std::sync::OnceLock::new();

fn main() {
    let code = match run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("ro_launcher: {e:#}");
            1
        }
    };
    if let Some(slot) = LOG_HANDLE.get() {
        if let Some(log) = slot.lock().unwrap().as_ref() {
            log.finish();
        }
    }
    std::process::exit(code);
}

fn run() -> Result<()> {
    let args = parse_args()?;
    if args.help {
        println!("{HELP}");
        return Ok(());
    }

    let (real_root, cfg_opt) = config::discover(args.root.as_deref());
    let from_file = cfg_opt.is_some();
    let cfg = cfg_opt.unwrap_or_default();
    let res = Resolved {
        root: real_root.clone(),
        cfg,
    };

    let log = Logger::new(true, false, None);
    let log = std::sync::Arc::new(log);
    LOG_HANDLE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap()
        .replace(log.clone());

    log.step(format!("ro_launcher - real root {}", real_root.display()));
    if !from_file {
        log.step("No ro_launcher.toml found - using built-in defaults");
    }
    log.step("console commands: s = status, r <login|char|web|map> = restart, q = quit");

    if args.dry_run {
        print_plan(&res, &log);
        log.step("Dry run: nothing was started, nothing was modified.");
        return Ok(());
    }

    if !args.no_elevate && !elevate::is_elevated() {
        log.step("Ragnarok.exe requires administrator rights - requesting elevation (UAC)...");
        match elevate::relaunch_elevated() {
            Ok(()) => {
                log.step("Re-launched with elevation; this window will close.");
                return Ok(());
            }
            Err(e) => {
                log.warn(format!(
                    "{e:#} - continuing without elevation; Ragnarok.exe may fail to start"
                ));
            }
        }
    }

    let symlink_root = match ensure_symlink(&real_root, &log) {
        Ok(p) => p,
        Err(e) => {
            if !real_root.to_string_lossy().contains(' ') {
                log.warn(format!(
                    "Junction creation failed ({e:#}); falling back to real root (no spaces detected)"
                ));
                real_root.clone()
            } else {
                log.error(format!(
                    "Real root contains spaces and junction creation failed: {e:#}\n\
                     Cannot proceed - please ensure the path has no spaces or run from a space-free location."
                ));
                return Err(e);
            }
        }
    };

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let lock_path = acquire_lock(&symlink_root, &exe_dir)?;

    log.step(format!("Using space-free root: {}", symlink_root.display()));

    let effective_res = Resolved {
        root: symlink_root.clone(),
        cfg: res.cfg,
    };

    let job = Job::new()?;
    let reg = Registry::new();
    let ctrl_c = std::sync::Arc::new(AtomicBool::new(false));
    {
        let reg = reg.clone();
        let flag = ctrl_c.clone();
        let _ = ctrlc::set_handler(move || {
            if !flag.swap(true, Ordering::SeqCst) {
                reg.begin_shutdown();
            }
        });
    }

    console::spawn((*log).clone(), reg.clone());

    let db = database::start_database(&effective_res, &job, &log, &reg);
    let db = match db {
        Ok(db) => db,
        Err(e) => {
            shutdown(&effective_res, &job, &reg, &log);
            release_lock(&lock_path)?;
            cleanup_symlink(&symlink_root, &exe_dir, &log);
            return Err(e);
        }
    };
    log.step(format!("Database step 1 complete via {}", db.mode_used.text()));

    server::start_all_servers(&effective_res, &job, &log, &reg)?;

    let mut client = match client::start_client(&effective_res, &job, &log, &reg) {
        Ok(c) => c,
        Err(e) => {
            log.error(format!("Ragnarok.exe could not be started: {e:#}"));
            shutdown(&effective_res, &job, &reg, &log);
            release_lock(&lock_path)?;
            cleanup_symlink(&symlink_root, &exe_dir, &log);
            return Err(e);
        }
    };

    let mut ports: Vec<(String, u16)> = vec![(
        "MariaDB".to_string(),
        effective_res.cfg.laragon.db_port,
    )];
    for spec in &effective_res.cfg.servers {
        if let Some(p) = spec.ready_port {
            ports.push((spec.label.clone(), p));
        }
    }
    log.step("Listening ports");
    gate::print_ports(&log, "127.0.0.1", &ports);

    let Some(status) = client::wait_for_exit(&mut client, &reg, &log) else {
        log.step("Ragnarok.exe closed");
        shutdown(&effective_res, &job, &reg, &log);
        release_lock(&lock_path)?;
        cleanup_symlink(&symlink_root, &exe_dir, &log);
        return Ok(());
    };
    let code = status.code().unwrap_or(-1);
    log.step(format!(
        "Ragnarok.exe exited (pid {}, code {code}) - shutting down all services",
        client.pid
    ));

    shutdown(&effective_res, &job, &reg, &log);
    release_lock(&lock_path)?;
    cleanup_symlink(&symlink_root, &exe_dir, &log);
    Ok(())
}

fn print_plan(res: &Resolved, log: &Logger) {
    log.step("Boot plan");
    log.step(format!(
        "  1. database: mysqld.exe direct (wait for MariaDB {}:{})",
        res.cfg.laragon.db_host, res.cfg.laragon.db_port
    ));
    log.step(format!("       mysqld.exe   {}", res.mysql_exe().display()));
    for spec in &res.cfg.servers {
        log.step(format!(
            "  2. {:<20} cwd={} port={:?} restart={}",
            spec.exe,
            res.server_cwd(spec).display(),
            spec.ready_port,
            if spec.restart.as_bool() { "on" } else { "off" }
        ));
    }
    log.step(format!("  3. {}", res.client_exe().display()));
}

fn shutdown(res: &Resolved, job: &Job, reg: &RegistryHandle, log: &Logger) {
    reg.begin_shutdown();
    log.step("Stopping all services");

    let mut entries = reg.snapshot();
    entries.sort_by(|a, b| b.1.order.cmp(&a.1.order));

    for (key, entry) in &entries {
        let Some(tracked) = entry.tracked.as_ref() else {
            log.notice(format!("{} already stopped", entry.label));
            continue;
        };
        if !tracked.is_alive() {
            log.notice(format!("{} already stopped", entry.label));
            reg.set_state(key, State::Exited(0));
            continue;
        }
        match tracked.terminate() {
            TermResult::Terminated => log.step(format!("Stopped {}", entry.label)),
            TermResult::AlreadyGone => log.notice(format!("{} already stopped", entry.label)),
            TermResult::Recycled => log.warn(format!(
                "{} (pid {}) already exited and its pid was reused - leaving it alone",
                entry.label, tracked.pid
            )),
            TermResult::Failed(e) => log.error(format!("{}: {e}", entry.label)),
        }
    }

    database::graceful_shutdown(res, log);

    log.ok("All services stopped.");

    log.step("Final safety net: terminating leftover processes in the job");
    match job.terminate_all() {
        Ok(()) => log.step("Stopped background processes"),
        Err(e) => log.warn(format!("job cleanup: {e:#}")),
    }
}