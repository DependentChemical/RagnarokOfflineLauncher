use anyhow::{anyhow, Context, Result};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::job::Job;
use crate::logging::{sanitize, Logger};

/// Prevents a console window from ever flashing for a child process.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A spawned process we are tracking.
#[derive(Debug, Clone)]
pub struct Tracked {
    pub pid: u32,
    /// Process creation time; proves the PID has not been recycled.
    pub start_time: u64,
}

impl Tracked {
    pub fn new(pid: u32) -> Tracked {
        let start_time = start_time_of(pid).unwrap_or(0);
        Tracked { pid, start_time }
    }

    /// True only when the PID exists *and* is the same process we started.
    pub fn is_alive(&self) -> bool {
        match lookup(self.pid) {
            Some(start) => self.start_time == 0 || start == self.start_time,
            None => false,
        }
    }

    /// Smart termination: never kills an already-dead process (silently) and
    /// never kills an unrelated process that recycled the PID.
    pub fn terminate(&self) -> TermResult {
        match lookup(self.pid) {
            None => TermResult::AlreadyGone,
            Some(start) => {
                if self.start_time != 0 && start != self.start_time {
                    TermResult::Recycled
                } else {
                    match kill_pid(self.pid) {
                        Ok(()) => {
                            let deadline = Instant::now() + Duration::from_secs(3);
                            while Instant::now() < deadline {
                                if !self.is_alive() {
                                    return TermResult::Terminated;
                                }
                                thread::sleep(Duration::from_millis(100));
                            }
                            TermResult::Failed(format!("pid {} survived TerminateProcess", self.pid))
                        }
                        Err(_) => {
                            // It exited between the liveness check and the kill:
                            // the desired end state, so report it as such.
                            if !self.is_alive() {
                                TermResult::AlreadyGone
                            } else {
                                TermResult::Failed(format!(
                                    "could not terminate pid {} (still running)",
                                    self.pid
                                ))
                            }
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum TermResult {
    Terminated,
    /// Process was not running any more - expected and silently tolerated.
    AlreadyGone,
    /// PID now belongs to a different, unrelated process.
    Recycled,
    Failed(String),
}

/// Spawns a GUI app (no pipes, no console) and joins it to the job.
pub fn spawn_detached(exe: &Path, cwd: &Path, job: &Job) -> Result<Child> {
    let mut cmd = Command::new(exe);
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let child = cmd
        .spawn()
        .with_context(|| format!("spawning {}", exe.display()))?;
    if let Err(e) = job.assign(child.id()) {
        // Not fatal: we still track and terminate the process explicitly.
        eprintln!("ro_launcher: warning: could not join job: {e:#}");
    }
    invalidate_cache();
    Ok(child)
}

/// Spawns a console app with its stdout/stderr captured and re-emitted into the
/// single launcher console as `[label] line`.
pub fn spawn_captured(
    exe: &Path,
    cwd: &Path,
    job: &Job,
    log: &Logger,
    label: &str,
) -> Result<(Child, Tracked)> {
    let mut cmd = Command::new(exe);
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning {}", exe.display()))?;
    if let Err(e) = job.assign(child.id()) {
        eprintln!("ro_launcher: warning: could not join job: {e:#}");
    }

    if let Some(out) = child.stdout.take() {
        pipe_reader(out, log.clone(), label.to_string(), "stdout");
    }
    if let Some(err) = child.stderr.take() {
        pipe_reader(err, log.clone(), label.to_string(), "stderr");
    }
    invalidate_cache();
    let tracked = Tracked::new(child.id());
    Ok((child, tracked))
}

/// Dedicated thread per pipe; emits one consolidated line per source line and
/// flushes any trailing partial line at EOF so nothing is lost.
fn pipe_reader<R: std::io::Read + Send + 'static>(
    reader: R,
    log: Logger,
    label: String,
    which: &'static str,
) {
    let _ = thread::Builder::new()
        .name(format!("pipe-{label}-{which}"))
        .spawn(move || {
            let mut br = BufReader::new(reader);
            let mut buf: Vec<u8> = Vec::with_capacity(512);
            loop {
                buf.clear();
                match br.read_until(b'\n', &mut buf) {
                    Ok(0) => break,
                    Ok(_) => {
                        let text = String::from_utf8_lossy(&buf);
                        let line = sanitize(&text);
                        if !line.is_empty() {
                            log.proc(&label, line);
                        }
                    }
                    Err(_) => break,
                }
            }
        });
}

/// Same as [`spawn_captured`] but with explicit arguments.
pub fn spawn_captured_with_args(
    exe: &Path,
    cwd: &Path,
    job: &Job,
    log: &Logger,
    label: &str,
    args: &[String],
) -> Result<(Child, Tracked)> {
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning {}", exe.display()))?;
    if let Err(e) = job.assign(child.id()) {
        eprintln!("ro_launcher: warning: could not join job: {e:#}");
    }
    if let Some(out) = child.stdout.take() {
        pipe_reader(out, log.clone(), label.to_string(), "stdout");
    }
    if let Some(err) = child.stderr.take() {
        pipe_reader(err, log.clone(), label.to_string(), "stderr");
    }
    invalidate_cache();
    let tracked = Tracked::new(child.id());
    Ok((child, tracked))
}

pub fn find_by_name(name: &str) -> Option<(u32, u64)> {
    let name = name.to_ascii_lowercase();
    with_cache(|sys| {
        for (pid, proc) in sys.processes() {
            if proc.name().to_string_lossy().to_ascii_lowercase() == name {
                return Some((pid.as_u32(), proc.start_time()));
            }
        }
        None
    })
}

/// Returns the image name of a live process, if any.
pub fn process_name(pid: u32) -> Option<String> {
    with_cache(|sys| {
        sys.process(sysinfo::Pid::from_u32(pid))
            .map(|p| p.name().to_string_lossy().to_string())
    })
}

/// True only if pid is a live `ro_launcher.exe`.
pub fn is_ro_launcher(pid: u32) -> bool {
    // Force a fresh table so a stale entry can't masquerade.
    invalidate_cache();
    process_name(pid)
        .map(|n| n.eq_ignore_ascii_case("ro_launcher.exe"))
        .unwrap_or(false)
}

/// Shared, periodically refreshed process table.
///
/// `System::new()` + `refresh_processes(All)` walks every process on the
/// machine; doing that per call (the supervisors poll ~10x/s each) pins a core
/// and makes the whole launcher sluggish. One background thread refreshes a
/// single instance instead.
/// Shared, periodically refreshed process table.
///
/// `System::new()` + `refresh_processes` walks every process on the machine;
/// doing that per call (the supervisors poll ~10x/s each) pins a core and makes
/// the whole launcher sluggish. One cached instance is refreshed on demand
/// instead. The static is declared once so `invalidate_cache()` can reach it.
type CacheCell = std::sync::Mutex<(std::time::Instant, sysinfo::System)>;

static CACHE: std::sync::OnceLock<CacheCell> = std::sync::OnceLock::new();

const CACHE_TTL: Duration = Duration::from_millis(1500);

/// Forces the next lookup to re-read the process table.
///
/// Must be called right after a spawn: the shared cache can be up to 1.5 s old
/// and would report a brand-new process as "not running".
pub fn invalidate_cache() {
    if let Some(cell) = CACHE.get() {
        if let Ok(mut guard) = cell.lock() {
            guard.0 = std::time::Instant::now() - CACHE_TTL * 2;
        }
    }
}

/// A refresh that actually populates pid/name/start_time.
///
/// `System::new()` defaults to `ProcessRefreshKind::nothing()`, which leaves
/// newly seen processes without their start time - a process spawned a moment
/// ago then looks like it does not exist yet.
fn refresh(sys: &mut sysinfo::System) {
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::everything(),
    );
}

fn with_cache<T>(f: impl FnOnce(&sysinfo::System) -> T) -> T {
    let cell = CACHE.get_or_init(|| {
        let mut sys = sysinfo::System::new();
        refresh(&mut sys);
        std::sync::Mutex::new((std::time::Instant::now(), sys))
    });
    let mut guard = cell.lock().unwrap();
    if guard.0.elapsed() >= CACHE_TTL {
        refresh(&mut guard.1);
        guard.0 = std::time::Instant::now();
    }
    f(&guard.1)
}

fn lookup(pid: u32) -> Option<u64> {
    with_cache(|sys| {
        sys.process(sysinfo::Pid::from_u32(pid))
            .map(|p| p.start_time())
    })
}

fn start_time_of(pid: u32) -> Option<u64> {
    lookup(pid)
}

fn kill_pid(pid: u32) -> Result<()> {
    // Always re-read the table fresh before killing: a cached entry could be
    // stale and the PID might already belong to someone else.
    let mut sys = sysinfo::System::new();
    refresh(&mut sys);
    let p = sys
        .process(sysinfo::Pid::from_u32(pid))
        .ok_or_else(|| anyhow!("pid {pid} not found"))?;
    if p.kill() {
        Ok(())
    } else {
        Err(anyhow!("kill pid {pid} was rejected"))
    }
}

// ---------------------------------------------------------------------------
// Window hiding (Laragon is a GUI app; we do not want a second window)
// ---------------------------------------------------------------------------

#[cfg(windows)]
pub fn hide_windows_of(pid: u32) {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{LPARAM, TRUE};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, ShowWindow, SW_HIDE,
    };

    unsafe extern "system" fn callback(
        hwnd: windows::Win32::Foundation::HWND,
        lparam: LPARAM,
    ) -> BOOL {
        let target = lparam.0 as u32;
        let mut owner = 0u32;
        if GetWindowThreadProcessId(hwnd, Some(&mut owner)) != 0 && owner == target {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        TRUE
    }

    unsafe {
        let _ = EnumWindows(Some(callback), LPARAM(pid as isize));
    }
}

#[cfg(not(windows))]
pub fn hide_windows_of(_pid: u32) {}

/// Diagnostics for GUI apps: reports the first top-level window a process owns
/// and whether it is currently visible, so "the game never opened a window" can
/// be told apart from "the window exists but is hidden".
#[cfg(windows)]
pub fn window_report(pid: u32) -> Option<(usize, bool)> {
    use std::sync::{Mutex, OnceLock};
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM, TRUE};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, IsWindowVisible,
    };

    static SLOT: OnceLock<Mutex<Option<u32>>> = OnceLock::new();
    static FOUND: OnceLock<Mutex<Option<(usize, bool)>>> = OnceLock::new();
    let slot = SLOT.get_or_init(|| Mutex::new(None));
    let found = FOUND.get_or_init(|| Mutex::new(None));
    *slot.lock().unwrap() = Some(pid);
    *found.lock().unwrap() = None;

    unsafe extern "system" fn cb(hwnd: HWND, _lparam: LPARAM) -> BOOL {
        let slot = SLOT.get_or_init(|| Mutex::new(None)).lock().unwrap();
        let mut out = FOUND.get_or_init(|| Mutex::new(None)).lock().unwrap();
        if let Some(target) = *slot {
            let mut owner = 0u32;
            if GetWindowThreadProcessId(hwnd, Some(&mut owner)) != 0 && owner == target {
                let visible = IsWindowVisible(hwnd).as_bool();
                if out.is_none() || (visible && !out.unwrap().1) {
                    *out = Some((hwnd.0 as usize, visible));
                }
            }
        }
        TRUE
    }

    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(0));
    }
    found.lock().unwrap().clone()
}

#[cfg(not(windows))]
pub fn window_report(_pid: u32) -> Option<(usize, bool)> {
    None
}