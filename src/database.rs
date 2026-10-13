//! Step 1 - the database stack in `1_Database`.
//!
//! Runs `mysqld.exe` directly with its console captured.
//! No Laragon, no junction logic - the symlink handles space-free paths.

use anyhow::{anyhow, Result};
use std::path::Path;
use std::time::Duration;

use crate::config::Resolved;
use crate::gate::wait_for_port;
use crate::job::Job;
use crate::logging::Logger;
use crate::proc::{find_by_name, spawn_captured_with_args, Tracked};
use crate::registry::{Entry, RegistryHandle, State};

pub struct DbHandle {
    pub mode_used: ModeUsed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeUsed {
    DirectMysqld,
}

impl ModeUsed {
    pub fn text(self) -> &'static str {
        "mysqld.exe (direct)"
    }
}

pub fn start_database(
    res: &Resolved,
    job: &Job,
    log: &Logger,
    reg: &RegistryHandle,
) -> Result<DbHandle> {
    let tracked = run_mysqld(res, job, log, reg)?;
    gate(log, reg, res, &tracked, "mysqld.exe")?;
    Ok(DbHandle {
        mode_used: ModeUsed::DirectMysqld,
    })
}

/// Starts MariaDB with its console captured - `[mysqld.exe] ...` in our window.
fn run_mysqld(res: &Resolved, job: &Job, log: &Logger, reg: &RegistryHandle) -> Result<Tracked> {
    let exe = res.mysql_exe();
    if !exe.is_file() {
        return Err(anyhow!(
            "mysqld.exe not found at {} - set laragon.mysql_exe in ro_launcher.toml",
            exe.display()
        ));
    }
    if let Some((pid, _)) = find_by_name("mysqld.exe") {
        log.notice(format!("MariaDB is already running, pid={pid}"));
        let tracked = Tracked::new(pid);
        reg.upsert(
            "database",
            Entry {
                label: "mysqld.exe".into(),
                state: State::Running,
                tracked: Some(tracked.clone()),
                order: 0,
                restart: crate::config::RestartPolicy::Off,
            },
        );
        return Ok(tracked);
    }
    let cwd = exe.parent().unwrap_or(Path::new(".")).to_path_buf();
    let ini = res.mysql_ini();
    log.step("Running mysqld.exe");
    log.step(format!("  --defaults-file={}", ini.display()));
    let (_child, tracked) = spawn_captured_with_args(
        &exe,
        &cwd,
        job,
        log,
        "mysqld.exe",
        &["--defaults-file".to_string() + &ini.display().to_string()],
    )?;
    reg.upsert(
        "database",
        Entry {
            label: "mysqld.exe".into(),
            state: State::Starting,
            tracked: Some(tracked.clone()),
            order: 0,
            restart: crate::config::RestartPolicy::Off,
        },
    );
    Ok(tracked)
}

/// Asks MariaDB to shut down cleanly before the job force-kills it.
pub fn graceful_shutdown(res: &Resolved, log: &Logger) -> bool {
    if !res.cfg.laragon.graceful_db_shutdown {
        return false;
    }
    let admin = res.mysql_admin_exe();
    if !admin.is_file() {
        log.warn(format!(
            "mariadb-admin.exe not found at {} - skipping clean database shutdown",
            admin.display()
        ));
        return false;
    }
    let user = res.cfg.laragon.db_user.clone();
    let password = res.cfg.laragon.db_password.clone();
    let ini = res.mysql_ini();

    let mut cmd = std::process::Command::new(&admin);
    cmd.arg(format!("--defaults-file={}", ini.display()))
        .arg("--protocol=TCP")
        .arg("-h")
        .arg(&res.cfg.laragon.db_host)
        .arg("-P")
        .arg(res.cfg.laragon.db_port.to_string())
        .arg("-u")
        .arg(&user)
        .arg("shutdown")
        .current_dir(admin.parent().unwrap_or(Path::new(".")))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    if !password.is_empty() {
        cmd.arg(format!("-p{password}"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }

    log.step("Stopping MariaDB cleanly (mariadb-admin shutdown)");
    let out = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            log.warn(format!("mariadb-admin could not run: {e}"));
            return false;
        }
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        log.warn(format!(
            "mariadb-admin shutdown failed ({}): {err}",
            out.status
        ));
        return false;
    }

    let host = res.cfg.laragon.db_host.clone();
    let port = res.cfg.laragon.db_port;
    for _ in 0..40 {
        if !crate::gate::port_is_open(&host, port) {
            log.ok("MariaDB stopped cleanly");
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    log.warn("MariaDB did not release port 3306 within 20s");
    false
}

fn gate(
    log: &Logger,
    reg: &RegistryHandle,
    res: &Resolved,
    tracked: &Tracked,
    what: &str,
) -> Result<()> {
    let host = res.cfg.laragon.db_host.clone();
    let port = res.cfg.laragon.db_port;
    let ready = wait_for_port(
        log,
        &host,
        port,
        res.cfg.laragon.ready_timeout_secs,
        &format!("{what} (MariaDB {host}:{port})"),
    );
    if !ready {
        log.error(format!(
            "{what}: MariaDB did not come up on {host}:{port} within {}s",
            res.cfg.laragon.ready_timeout_secs
        ));
        if !tracked.is_alive() {
            log.error(format!("{what} is no longer running - see its output above."));
        } else {
            log.error(
                "Check 1_Database/bin/mysql/*/my.ini (datadir=), \
                 and that nothing else already holds port 3306.",
            );
        }
        return Err(anyhow!("database never became ready"));
    }
    log.ok(format!(
        "{what} Running (pid={}) - MariaDB ready on {host}:{port}",
        tracked.pid
    ));
    reg.set_state("database", State::Running);
    Ok(())
}