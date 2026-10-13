//! Step 1 - run laragon.exe from the symlink path.
//! Laragon manages MariaDB + Apache + Nginx internally.

use anyhow::{anyhow, Result};
use std::path::Path;
use std::thread;
use std::time::Duration;

use crate::config::Resolved;
use crate::gate::wait_for_port;
use crate::job::Job;
use crate::logging::Logger;
use crate::proc::{find_by_name, hide_windows_of, spawn_detached, Tracked};
use crate::registry::{Entry, RegistryHandle, State};

pub struct DbHandle;

pub fn start_database(
    res: &Resolved,
    job: &Job,
    log: &Logger,
    reg: &RegistryHandle,
) -> Result<DbHandle> {
    let exe = res.laragon_exe();
    if !exe.is_file() {
        return Err(anyhow!(
            "laragon.exe not found at {} - check 1_Database/laragon.exe exists",
            exe.display()
        ));
    }

    if let Some((pid, _)) = find_by_name("laragon.exe") {
        log.notice(format!("Laragon already running, pid={pid}"));
        let tracked = Tracked::new(pid);
        reg.upsert(
            "laragon",
            Entry {
                label: "laragon.exe".into(),
                state: State::Running,
                tracked: Some(tracked.clone()),
                order: 0,
                restart: crate::config::RestartPolicy::On,
            },
        );
        gate(log, reg, res, &tracked)?;
        return Ok(DbHandle);
    }

    let cwd = exe.parent().unwrap_or(Path::new(".")).to_path_buf();
    log.step("Running laragon.exe");
    let mut child = spawn_detached(&exe, &cwd, job)?;
    let tracked = Tracked::new(child.id());
    reg.upsert(
        "laragon",
        Entry {
            label: "laragon.exe".into(),
            state: State::Starting,
            tracked: Some(tracked.clone()),
            order: 0,
            restart: crate::config::RestartPolicy::On,
        },
    );

    if res.cfg.laragon.hide_window {
        let pid = child.id();
        thread::spawn(move || {
            for delay in [0u64, 300, 1500] {
                if delay > 0 {
                    thread::sleep(Duration::from_millis(delay));
                }
                hide_windows_of(pid);
            }
        });
    }
    thread::spawn(move || {
        let _ = child.wait();
    });

    gate(log, reg, res, &tracked)?;
    Ok(DbHandle)
}

fn gate(
    log: &Logger,
    reg: &RegistryHandle,
    res: &Resolved,
    tracked: &Tracked,
) -> Result<()> {
    let host = res.cfg.laragon.db_host.clone();
    let port = res.cfg.laragon.db_port;
    let ready = wait_for_port(
        log,
        &host,
        port,
        res.cfg.laragon.ready_timeout_secs,
        &format!("laragon.exe (MariaDB {host}:{port})"),
    );
    if !ready {
        log.error(format!(
            "laragon.exe: MariaDB did not come up on {host}:{port} within {}s",
            res.cfg.laragon.ready_timeout_secs
        ));
        if !tracked.is_alive() {
            log.error("laragon.exe is no longer running - see output above.");
        }
        return Err(anyhow!("database never became ready"));
    }
    log.ok(format!(
        "laragon.exe Running (pid={}) - MariaDB ready on {host}:{port}",
        tracked.pid
    ));
    reg.set_state("laragon", State::Running);
    Ok(())
}

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

    let mut cmd = std::process::Command::new(&admin);
    cmd.arg("--protocol=TCP")
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
        log.warn(format!("mariadb-admin shutdown failed ({}): {err}", out.status));
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