use crate::config::RestartPolicy;
use crate::proc::Tracked;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Starting,
    Running,
    /// Crashed and waiting out the serv.bat restart delay.
    WaitingRestart(u64),
    /// Exited and will not be restarted (restart_mode=off, or shutting down).
    Exited(i32),
    Stopping,
}

impl State {
    pub fn text(&self) -> String {
        match self {
            State::Starting => "starting".into(),
            State::Running => "running".into(),
            State::WaitingRestart(s) => format!("restarting in {s}s"),
            State::Exited(c) => format!("exited ({c})"),
            State::Stopping => "stopping".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// Image name; also the `[...]` console prefix.
    pub label: String,
    pub state: State,
    pub tracked: Option<Tracked>,
    /// Start order, so shutdown can walk it in reverse.
    pub order: u32,
    pub restart: RestartPolicy,
}

/// Live view of everything the launcher owns.
pub struct Registry {
    entries: Mutex<BTreeMap<String, Entry>>,
    pub shutting_down: AtomicBool,
    /// `notify_all` bumps this; it is also the condvar's mutex.
    pub wake_tx: Mutex<u64>,
    pub wake: Condvar,
    /// Per-key "restart me now" requests raised by the `r <key>` command.
    flags: Mutex<HashMap<String, bool>>,
}

pub type RegistryHandle = Arc<Registry>;

impl Registry {
    pub fn new() -> RegistryHandle {
        Arc::new(Registry {
            entries: Mutex::new(BTreeMap::new()),
            shutting_down: AtomicBool::new(false),
            wake_tx: Mutex::new(0),
            wake: Condvar::new(),
            flags: Mutex::new(HashMap::new()),
        })
    }

    pub fn upsert(&self, key: &str, entry: Entry) {
        self.entries.lock().unwrap().insert(key.to_string(), entry);
    }

    pub fn set_state(&self, key: &str, state: State) {
        let mut map = self.entries.lock().unwrap();
        if let Some(e) = map.get_mut(key) {
            e.state = state;
        }
    }

    pub fn set_tracked(&self, key: &str, tracked: Option<Tracked>) {
        let mut map = self.entries.lock().unwrap();
        if let Some(e) = map.get_mut(key) {
            e.tracked = tracked;
        }
    }

    /// All entries in start order (oldest first).
    pub fn snapshot(&self) -> Vec<(String, Entry)> {
        let map = self.entries.lock().unwrap();
        let mut v: Vec<(String, Entry)> = map
            .iter()
            .map(|(k, e)| (k.clone(), e.clone()))
            .collect();
        v.sort_by_key(|(_, e)| e.order);
        v
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    /// Blocks until a supervised server reports `Running`, it dies, or the
    /// timeout expires. Used to keep the boot order strictly sequential: the
    /// next server (and finally the client) must not start before this one is
    /// actually listening.
    pub fn wait_for_running(&self, key: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.is_shutting_down() {
                return false;
            }
            if let Some((_, entry)) = self
                .snapshot()
                .into_iter()
                .find(|(k, _)| k == key)
            {
                match entry.state {
                    State::Running => return true,
                    State::Exited(_) => return false,
                    _ => {}
                }
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn begin_shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.notify_all();
    }

    pub fn notify_all(&self) {
        let mut g = self.wake_tx.lock().unwrap();
        *g += 1;
        self.wake.notify_all();
    }

    /// `r <key>`: ask a supervisor to skip its restart delay.
    pub fn request_restart(&self, key: &str) {
        self.flags.lock().unwrap().insert(key.to_string(), true);
        self.notify_all();
    }

    fn take_flag(&self, key: &str) -> bool {
        self.flags.lock().unwrap().remove(key).unwrap_or(false)
    }

    /// Cancelable sleep used for the serv.bat "Restarting in 15 seconds" pause.
    /// Returns `true` if the wait completed normally, `false` on shutdown.
    /// When `key` is `Some`, a pending `r <key>` request short-circuits it.
    pub fn interruptible_sleep(&self, secs: u64, key: Option<&str>) -> bool {
        let start = Instant::now();
        let total = Duration::from_secs(secs);
        let mut seen = *self.wake_tx.lock().unwrap();
        loop {
            if self.is_shutting_down() {
                return false;
            }
            if let Some(k) = key {
                if self.take_flag(k) {
                    return true;
                }
            }
            let elapsed = start.elapsed();
            if elapsed >= total {
                return !self.is_shutting_down();
            }
            let (g, _) = self
                .wake
                .wait_timeout(self.wake_tx.lock().unwrap(), total - elapsed)
                .unwrap();
            let now = *g;
            drop(g);
            if now != seen {
                seen = now;
                if let Some(k) = key {
                    if self.take_flag(k) {
                        return true;
                    }
                }
            }
        }
    }
}