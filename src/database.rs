//! Step 1 - the database stack in `1_Database`.
//!
//! Two ways to bring MariaDB up, because Laragon has a hard limitation:
//! **Laragon refuses to run from a path containing a space** ("Laragon path
//! must not have SPACE"). It also starts every service through `cmd.exe`, so a
//! space in `D:\NewGaems\Ragnarok Offline\...` breaks its batch commands.
//!
//!  * `laragon` - run `laragon.exe` (GUI, hidden). Needs a space-free path; the
//!    launcher creates a directory junction to get one (see `portable::link`).
//!  * `direct`  - run `mysqld.exe` ourselves with the captured console, which is
//!    what the rAthena servers actually need. No space limitation at all, and
//!    MariaDB's own startup log lands in the one console.
//!  * `auto`    - `laragon` when the path is space-free or a junction can be
//!    created, `direct` otherwise.
//!
//! Either way we gate on MariaDB's TCP port before touching the servers.

use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crate::config::{DbMode, Resolved};
use crate::gate::wait_for_port;
use crate::job::Job;
use crate::logging::Logger;
use crate::portable;
use crate::proc::{find_by_name, hide_windows_of, spawn_captured_with_args, spawn_detached, Tracked};
use crate::registry::{Entry, RegistryHandle, State};

/// Internal decision after applying the `auto` rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveDbMode {
    Laragon,
    Direct,
}

pub struct DbHandle {
    pub mode_used: ModeUsed,
}

/// Which route the launcher actually took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeUsed {
    Laragon,
    LaragonViaLink,
    DirectMysqld,
}

impl ModeUsed {
    pub fn text(self) -> &'static str {
        match self {
            ModeUsed::Laragon => "laragon.exe",
            ModeUsed::LaragonViaLink => "laragon.exe (via space-free junction)",
            ModeUsed::DirectMysqld => "mysqld.exe (direct)",
        }
    }
}

pub fn start_database(
    res: &Resolved,
    job: &Job,
    log: &Logger,
    reg: &RegistryHandle,
    decision: (ResolveDbMode, Option<PathBuf>),
) -> Result<DbHandle> {
    let (mode, link_root) = decision;
    match mode {
        ResolveDbMode::Laragon => {
            let tracked = run_laragon(res, link_root.as_deref(), job, log, reg)?;
            gate(log, reg, res, &tracked, "laragon.exe")?;
            Ok(DbHandle {
                mode_used: if link_root.is_some() {
                    ModeUsed::LaragonViaLink
                } else {
                    ModeUsed::Laragon
                },
            })
        }
        ResolveDbMode::Direct => {
            let tracked = run_mysqld(res, job, log, reg)?;
            gate(log, reg, res, &tracked, "mysqld.exe")?;
            Ok(DbHandle {
                mode_used: ModeUsed::DirectMysqld,
            })
        }
    }
}

/// Decides how the database stack will be started, creating the space-free
/// junction when one is needed. Run this *before* the portability repair so
/// that Laragon's config files are rewritten to the very root Laragon will see
/// (otherwise Laragon reads a path with a space and its cmd.exe service
/// commands fail with "is not recognized as an internal or external command").
pub fn decide(res: &Resolved, log: &Logger) -> (ResolveDbMode, Option<PathBuf>) {
    decide_mode(res, log)
}

/// The root the whole stack should be treated as: the junction when Laragon
/// runs through it, otherwise the real root.
pub fn effective_root(res: &Resolved, link: Option<&Path>) -> PathBuf {
    link.map(|p| p.to_path_buf()).unwrap_or_else(|| res.root.clone())
}

fn decide_mode(res: &Resolved, log: &Logger) -> (ResolveDbMode, Option<PathBuf>) {
    let has_space = res.root.to_string_lossy().contains(' ');
    match res.cfg.laragon.mode {
        DbMode::Direct => (ResolveDbMode::Direct, None),
        DbMode::Laragon => {
            if has_space {
                match portable::space_free_link(&res.root, &res.cfg.laragon.link_path, log) {
                    Ok(link) => {
                        log.step(format!(
                            "Root path contains a space; using junction {} -> {}",
                            link.display(),
                            res.root.display()
                        ));
                        (ResolveDbMode::Laragon, Some(link))
                    }
                    Err(e) => {
                        log.warn(format!(
                            "Laragon cannot run from a path with a space and the junction \
                             could not be created ({e:#}); starting MariaDB directly instead"
                        ));
                        (ResolveDbMode::Direct, None)
                    }
                }
            } else {
                (ResolveDbMode::Laragon, None)
            }
        }
        DbMode::Auto => {
            if !has_space {
                (ResolveDbMode::Laragon, None)
            } else if let Ok(link) =
                portable::space_free_link(&res.root, &res.cfg.laragon.link_path, log)
            {
                log.step(format!(
                    "Root path contains a space; using junction {} -> {}",
                    link.display(),
                    res.root.display()
                ));
                (ResolveDbMode::Laragon, Some(link))
            } else {
                log.step(
                    "Root path contains a space; starting MariaDB directly instead of Laragon",
                );
                (ResolveDbMode::Direct, None)
            }
        }
    }
}

fn run_laragon(
    res: &Resolved,
    link_root: Option<&Path>,
    job: &Job,
    log: &Logger,
    reg: &RegistryHandle,
) -> Result<Tracked> {
    // Through the junction, Laragon sees (and stores) a space-free root.
    let root = link_root.unwrap_or(&res.root);
    let exe = root.join("1_Database").join("laragon.exe");
    if !exe.is_file() {
        return Err(anyhow!("{} not found", exe.display()));
    }
    let cwd = exe.parent().unwrap_or(Path::new(".")).to_path_buf();
    let name = "laragon.exe";

    log.step(format!("Running {name}"));
    let mut child = spawn_detached(&exe, &cwd, job)?;
    let tracked = Tracked::new(child.id());
    reg.upsert(
        "laragon",
        Entry {
            label: name.into(),
            state: State::Starting,
            tracked: Some(tracked.clone()),
            order: 0,
            restart: crate::config::RestartPolicy::On,
        },
    );

    if res.cfg.laragon.hide_window {
        // std::process cannot set STARTUPINFO.wShowWindow, so we hide whatever
        // windows the process owns right after it creates them. Laragon builds
        // its tray window asynchronously, hence the retries.
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
    Ok(tracked)
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
///
/// A hard kill leaves MyISAM tables flagged "marked as crashed and should be
/// repaired" (ragnarok.login, vendings, buyingstores, ...) and leaves InnoDB to
/// run crash recovery on the next boot. `mariadb-admin shutdown` avoids both.
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
        // Force TCP: `localhost` would make the client look for the unix
        // socket / named pipe from my.ini instead of the listening port.
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

    // Wait for the port to be released so we know the server is really gone.
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
                "Check 1_Database/usr/laragon.ini and 1_Database/bin/mysql/*/my.ini (datadir=), \
                 and that nothing else already holds port 3306.",
            );
        }
        let _ = reg;
        return Err(anyhow!("database never became ready"));
    }
    log.ok(format!(
        "{what} Running (pid={}) - MariaDB ready on {host}:{port}",
        tracked.pid
    ));
    reg.set_state(
        if what == "laragon.exe" { "laragon" } else { "database" },
        State::Running,
    );
    Ok(())
}