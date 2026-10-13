//! ro_launcher - one console for the whole RO_PreRenewal stack.
//!
//!   1_Database\laragon.exe   (MariaDB / Apache / Nginx, GUI, hidden)
//!   2_Server\{login,char,web,map}-server.exe  (console output captured)
//!   3_Client\Ragnarok.exe    (the session; closing it stops everything)

mod client;
mod clientreg;
mod config;
mod console;
mod gate;
mod job;
mod database;
mod elevate;
mod logging;
mod mariadb;
mod portable;
mod proc;
mod registry;
mod server;

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use config::Resolved;
use job::Job;
use logging::Logger;
use proc::TermResult;
use registry::{Registry, RegistryHandle, State};

struct Args {
    root: Option<PathBuf>,
    dry_run: bool,
    no_color: bool,
    timestamps: bool,
    no_log_file: bool,
    no_tail: bool,
    no_elevate: bool,
    help: bool,
}

fn parse_args() -> Result<Args> {
    let mut a = Args {
        root: None,
        dry_run: false,
        no_color: false,
        timestamps: false,
        no_log_file: false,
        no_tail: false,
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
            "--no-color" => a.no_color = true,
            "--timestamps" => a.timestamps = true,
            "--no-log-file" => a.no_log_file = true,
            "--no-tail" => a.no_tail = true,
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
    --no-color       Disable ANSI colors
    --timestamps     Prefix every line with HH:MM:SS.mmm
    --no-log-file    Do not tee the console to ro_launcher.log
    --no-tail        Do not tail MariaDB's .err file
    --no-elevate      Never re-launch through UAC (Ragnarok.exe requires it)
    -h, --help       This text

CONSOLE COMMANDS (while running):
    s            status table
    r <key>      restart a server (login | char | web | map)
    q            shutdown everything (same as closing Ragnarok.exe)
    Ctrl+C       shutdown everything
"#;

/// Set as soon as the logger exists so `main` can flush it before exiting.
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
    // Drain the console queue: `process::exit` does not unwind, so the writer
    // thread would be killed with the last lines still pending.
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

    let (root, cfg_opt) = config::discover(args.root.as_deref());
    let from_file = cfg_opt.is_some();
    let mut cfg = cfg_opt.unwrap_or_default();
    if args.no_color {
        cfg.colors = false;
    }
    if args.timestamps {
        cfg.timestamps = true;
    }
    if args.no_log_file {
        cfg.log_to_file = false;
    }
    let res = Resolved {
        root: root.clone(),
        cfg,
    };

    // Preflight runs before the logger so that failures are visible even if the
    // log file cannot be created.
    preflight(&res)?;

    let log = Logger::new(
        res.cfg.colors,
        res.cfg.timestamps,
        if res.cfg.log_to_file { Some(res.log_file()) } else { None },
    );
    let log = std::sync::Arc::new(log);
    LOG_HANDLE
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap()
        .replace(log.clone());

    log.step(format!("ro_launcher - root {}", res.root.display()));
    if !from_file {
        log.step("No ro_launcher.toml found - using the built-in defaults");
    }
    log.step(format!(
        "console commands: s = status, r <login|char|web|map> = restart, q = quit"
    ));

if args.dry_run {
        print_plan(&res, &log);
        let (mode, link) = database::decide(&res, &log);
        log.step(format!(
            "  resolved database mode: {mode:?}{}",
            link.as_ref()
                .map(|p| format!(" via {}", p.display()))
                .unwrap_or_default()
        ));
        report_stale_paths(&database::effective_root(&res, link.as_deref()), &res, &log);
        log.step("Dry run: nothing was started, nothing was modified.");
        return Ok(());
    }

    // Ragnarok.exe is marked requireAdministrator, so a medium-integrity
    // launcher could never spawn it. Ask for elevation once, up front.
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

    // Decide the database route first: if Laragon has to run through a
    // space-free junction, the portability repair must rewrite its config to
    // that junction path - rewriting to the real (space-containing) path would
    // break Laragon's own cmd.exe service commands again.
    let db_decision = database::decide(&res, &log);
    let effective_root = database::effective_root(&res, db_decision.1.as_deref());

    // Always-on portability repair (Laragon/my.ini keep absolute paths).
    repair_paths(&effective_root, &res, &log);

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

    // --- Step 1: the database stack ---------------------------------------
    let db = database::start_database(&res, &job, &log, &reg, db_decision);
    let db = match db {
        Ok(db) => db,
        Err(e) => {
            shutdown(&res, &job, &reg, &log);
            return Err(e);
        }
    };
    log.step(format!("Database step 1 complete via {}", db.mode_used.text()));

    // When Laragon owns MariaDB its console is unreachable, so its error log is
    // tailed instead and shown as `[mariadb.err]`. In direct mode the console
    // is already captured, so the tail would only duplicate it.
    if !args.no_tail
        && db.mode_used != database::ModeUsed::DirectMysqld
        && res.mariadb_dir().is_dir()
    {
        mariadb::spawn_tail(
            res.mariadb_dir(),
            res.cfg.mariadb_tail.label.clone(),
            res.cfg.mariadb_tail.poll_ms,
            (*log).clone(),
            reg.clone(),
        );
    }

    // --- Step 2 -----------------------------------------------------------
    server::start_all_servers(&res, &job, &log, &reg)?;

    // --- Step 3 -----------------------------------------------------------
    let mut client = match client::start_client(&res, &job, &log, &reg) {
        Ok(c) => c,
        Err(e) => {
            // Never leave the stack half-started behind us.
            log.error(format!("Ragnarok.exe could not be started: {e:#}"));
            shutdown(&res, &job, &reg, &log);
            return Err(e);
        }
    };
    let mut ports: Vec<(String, u16)> = vec![(
        "MariaDB".to_string(),
        res.cfg.laragon.db_port,
    )];
    for spec in &res.cfg.servers {
        if let Some(p) = spec.ready_port {
            ports.push((spec.label.clone(), p));
        }
    }
    log.step("Listening ports");
    gate::print_ports(&log, "127.0.0.1", &ports);
    let Some(status) = client::wait_for_exit(&mut client, &reg, &log) else {
        log.step("Ragnarok.exe closed");
        shutdown(&res, &job, &reg, &log);
        return Ok(());
    };
    let code = status.code().unwrap_or(-1);
    log.step(format!(
        "Ragnarok.exe exited (pid {}, code {code}) - shutting down all services",
        client.pid
    ));

    shutdown(&res, &job, &reg, &log);
    Ok(())
}

/// Validates every path we are going to use before anything is launched.
fn preflight(res: &Resolved) -> Result<()> {
    let mut missing: Vec<String> = Vec::new();
    let mut rows: Vec<(String, bool, String)> = Vec::new();

    for d in ["1_Database", "2_Server", "3_Client"] {
        let p = res.root.join(d);
        let ok = p.is_dir();
        if !ok {
            missing.push(p.display().to_string());
        }
        rows.push((
            format!("{d}/"),
            ok,
            if ok { "directory".into() } else { "MISSING".into() },
        ));
    }

    let mut files: Vec<(String, PathBuf)> = vec![("laragon.exe".into(), res.laragon_exe())];
    for spec in &res.cfg.servers {
        files.push((format!("2_Server/{}", spec.exe), res.server_exe(spec)));
    }
    files.push(("3_Client/Ragnarok.exe".into(), res.client_exe()));

    for (name, path) in files {
        let ok = path.is_file();
        if !ok {
            missing.push(path.display().to_string());
        }
        rows.push((
            name,
            ok,
            if ok { size_string(&path) } else { "MISSING".to_string() },
        ));
    }

    // rAthena writes log/login.log & friends; the directory must exist.
    let log_dir = res.root.join("2_Server").join("log");
    let log_ok = if log_dir.is_dir() {
        true
    } else {
        std::fs::create_dir_all(&log_dir).is_ok()
    };
    rows.push((
        "2_Server/log/".into(),
        log_ok,
        if log_ok { "ready".into() } else { "cannot create".into() },
    ));

    println!("Configuration check");
    println!("{}", "-".repeat(72));
    for (name, ok, detail) in &rows {
        println!(
            "{} {:<28} {}",
            if *ok { "[ok]  " } else { "[FAIL]" },
            name,
            detail
        );
    }
    println!("{}", "-".repeat(72));
    if !missing.is_empty() {
        bail!(
            "cannot start, {} required path(s) missing:\n  - {}",
            missing.len(),
            missing.join("\n  - ")
        );
    }
    Ok(())
}

fn size_string(path: &std::path::Path) -> String {
    std::fs::metadata(path)
        .map(|m| format!("{:.0} KB", m.len() as f64 / 1024.0))
        .unwrap_or_else(|_| "present".into())
}

/// Read-only variant used by `--dry-run`: lists files that would be rewritten.
fn report_stale_paths(root: &Path, res: &Resolved, log: &Logger) {
    let mut hits = 0usize;
    for rel in &res.cfg.portable_fix_dirs {
        let dir = config::join(root, rel);
        let mut files = Vec::new();
        portable::collect_public(&dir, 0, &mut files);
        for file in files {
            let Ok(bytes) = std::fs::read(&file) else { continue };
            let text: String = bytes.iter().map(|b| *b as char).collect();
            let (_, count) = portable::rewrite_public(&text, root);
            if count > 0 {
                hits += 1;
                if hits <= 8 {
                    log.step(format!(
                        "  would fix {count} stale path(s) in {}",
                        file.display()
                    ));
                }
            }
        }
    }
    if hits == 0 {
        log.step("  portable path check: nothing to fix");
    } else if hits > 8 {
        log.step(format!("  ... and {} more file(s)", hits - 8));
    }
}

/// Rewrites absolute paths left over from a previous install location so the
/// whole stack keeps working after the folder is moved.
fn repair_paths(root: &Path, res: &Resolved, log: &Logger) {
    let report = portable::repair_stale_paths(
        root,
        &res.cfg.portable_fix_dirs,
        &res.cfg.backup_suffix,
        log,
    );
    if report.rewritten.is_empty() {
        log.step(format!(
            "Portable path check: {} file(s) scanned, nothing to fix",
            report.scanned
        ));
    } else {
        for (file, count) in &report.rewritten {
            log.notice(format!(
                "Fixed {} stale absolute path(s) in {}",
                count,
                file.display()
            ));
        }
        log.step(format!(
            "Portable path check: rewrote {} of {} file(s) (backups: *{})",
            report.rewritten.len(),
            report.scanned,
            res.cfg.backup_suffix
        ));
    }
}

fn print_plan(res: &Resolved, log: &Logger) {
    log.step("Boot plan");
    log.step(format!(
        "  1. database stack: mode={:?}{} (wait for MariaDB {host}:{port})",
        res.cfg.laragon.mode,
        if res.root.to_string_lossy().contains(' ') {
            " - root path contains a space, Laragon will run through a junction or mysqld directly"
        } else {
            ""
        },
        host = res.cfg.laragon.db_host,
        port = res.cfg.laragon.db_port
    ));
    log.step(format!("       laragon.exe  {}", res.laragon_exe().display()));
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
    log.step(format!(
        "  tail: {} (label {})",
        res.mariadb_dir().display(),
        res.cfg.mariadb_tail.label
    ));
    log.step(format!(
        "  log file: {}",
        if res.cfg.log_to_file { res.log_file().display().to_string() } else { "disabled".into() }
    ));
}

/// runserver.bat ":Stop" - everything in reverse start order, then the job.
fn shutdown(res: &Resolved, job: &Job, reg: &RegistryHandle, log: &Logger) {
    reg.begin_shutdown();
    log.step("Stoping all serv");

    let mut entries = reg.snapshot();
    entries.sort_by(|a, b| b.1.order.cmp(&a.1.order));

    for (key, entry) in &entries {
        let Some(tracked) = entry.tracked.as_ref() else {
            log.notice(format!("{} already stopped", entry.label));
            continue;
        };
        if !tracked.is_alive() {
            // Smart: an app that closed on its own is silently skipped.
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

    // MariaDB first, gracefully: killing it corrupts MyISAM tables, so it gets a
    // clean `mariadb-admin shutdown` while the rest of the stack comes down.
    database::graceful_shutdown(res, log);

    // Everything we track is down and the database is closed: report success
    // *before* the job safety net, because terminating the job may take the
    // launcher itself down with it if it ended up inside the job.
    log.ok("All services stopped.");

    // Catch-all for anything we never tracked directly (nginx, apache, and any
    // process Laragon started that is still alive).
    log.step("Final safety net: terminating leftover processes in the job");
    match job.terminate_all() {
        Ok(()) => log.step("Stopped background web services (nginx / apache)"),
        Err(e) => log.warn(format!("job cleanup: {e:#}")),
    }
}