use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// How a supervised server reacts to an unexpected exit.
/// Mirrors `restart_mode` in `runserver.bat` / `serv.bat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RestartPolicy {
    /// `restart_mode=on` (runserver.bat "watch"): restart 15s after a crash.
    On,
    /// `restart_mode=off` (runserver.bat "start"): log the exit and stay down.
    Off,
}

impl RestartPolicy {
    pub fn as_bool(self) -> bool {
        matches!(self, RestartPolicy::On)
    }
}

/// How the database stack in `1_Database` is started.
///
/// Laragon hard-refuses a root path containing a space, so `Auto` transparently
/// falls back to running `mysqld.exe` directly (same MariaDB, same my.ini, but
/// its console output is captured too).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DbMode {
    Auto,
    Laragon,
    Direct,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LaragonSpec {
    /// Relative to the project root, e.g. `1_Database/laragon.exe`.
    pub exe: PathBuf,
    /// `auto` | `laragon` | `direct`
    pub mode: DbMode,
    /// Space-free junction used only when Laragon has to be run through one.
    /// `%DRIVE%`-relative absolute path, e.g. `D:\RO_Link`.
    pub link_path: PathBuf,
    /// Used by `direct` mode.
    pub mysql_exe: PathBuf,
    /// Used by `direct` mode.
    pub mysql_ini: PathBuf,
    /// MariaDB port we gate on before touching the servers.
    pub db_port: u16,
    pub db_host: String,
    /// How long to wait for MariaDB to accept connections.
    pub ready_timeout_secs: u64,
    /// `ShowWindow(SW_HIDE)` on every window the process owns.
    pub hide_window: bool,
    /// Ask MariaDB to shut down cleanly instead of killing it.
    pub graceful_db_shutdown: bool,
    /// Credentials used by mariadb-admin at shutdown.
    pub db_user: String,
    pub db_password: String,
    /// Used at shutdown; defaults next to mysqld.exe.
    pub mysql_admin_exe: PathBuf,
}

impl Default for LaragonSpec {
    fn default() -> Self {
        Self {
            exe: PathBuf::from("1_Database/laragon.exe"),
            mode: DbMode::Auto,
            link_path: PathBuf::from(r"D:\RO_Link"),
            mysql_exe: PathBuf::from("1_Database/bin/mysql/mariadb-11.8.8-winx64/bin/mysqld.exe"),
            mysql_ini: PathBuf::from("1_Database/bin/mysql/mariadb-11.8.8-winx64/my.ini"),
            db_port: 3306,
            db_host: "127.0.0.1".into(),
            ready_timeout_secs: 90,
            hide_window: true,
            graceful_db_shutdown: true,
            db_user: "root".into(),
            db_password: String::new(),
            mysql_admin_exe: PathBuf::from(
                "1_Database/bin/mysql/mariadb-11.8.8-winx64/bin/mariadb-admin.exe",
            ),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerSpec {
    /// Stable key used by the `r <key>` console command.
    pub key: String,
    pub exe: String,
    /// Human name used by serv.bat messages, e.g. `Login-Server`.
    pub label: String,
    /// Directory relative to the root that the process runs in (`2_Server`).
    pub cwd: PathBuf,
    /// Port that must accept connections before we call it "Running".
    pub ready_port: Option<u16>,
    pub restart: RestartPolicy,
}

impl Default for ServerSpec {
    fn default() -> Self {
        Self {
            key: "login".into(),
            exe: "login-server.exe".into(),
            label: "Login-Server".into(),
            cwd: PathBuf::from("2_Server"),
            ready_port: Some(6900),
            restart: RestartPolicy::On,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ClientSpec {
    pub exe: PathBuf,
    pub cwd: PathBuf,
}

impl Default for ClientSpec {
    fn default() -> Self {
        Self {
            exe: PathBuf::from("3_Client/Ragnarok.exe"),
            cwd: PathBuf::from("3_Client"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MariadbTailSpec {
    /// Directory (relative to root) holding MariaDB's `*.err` log.
    pub dir: PathBuf,
    /// Prefix shown in the consolidated console.
    pub label: String,
    pub poll_ms: u64,
}

impl Default for MariadbTailSpec {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("1_Database/data/mariadb-11.8"),
            label: "mariadb.err".into(),
            poll_ms: 500,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Path to the MariaDB `.err` file to tail (5th prefixed source).
    pub mariadb_tail: MariadbTailSpec,
    /// Directories scanned (recursively, text files only) when rewriting stale
    /// absolute paths left behind by a previous install location.
    pub portable_fix_dirs: Vec<PathBuf>,
    /// Backup suffix written next to every file we rewrite.
    pub backup_suffix: String,
    /// Seconds a crashed server waits before being restarted (serv.bat: 15).
    pub restart_delay_secs: u64,
    /// Per-server readiness timeout.
    pub server_ready_timeout_secs: u64,
    pub log_to_file: bool,
    pub log_file: PathBuf,
    pub colors: bool,
    pub timestamps: bool,
    pub laragon: LaragonSpec,
    /// Order matters: this mirrors `runserver.bat` (login, char, web, map).
    pub servers: Vec<ServerSpec>,
    pub client: ClientSpec,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mariadb_tail: MariadbTailSpec::default(),
            portable_fix_dirs: vec![
                PathBuf::from("1_Database/usr"),
                PathBuf::from("1_Database/etc"),
                PathBuf::from("1_Database/bin/laragon"),
                PathBuf::from("1_Database/bin/mysql"),
            ],
            backup_suffix: ".bak".into(),
            restart_delay_secs: 15,
            server_ready_timeout_secs: 30,
            log_to_file: true,
            log_file: PathBuf::from("ro_launcher.log"),
            colors: true,
            timestamps: false,
            laragon: LaragonSpec::default(),
            servers: vec![
                ServerSpec {
                    key: "login".into(),
                    exe: "login-server.exe".into(),
                    label: "Login-Server".into(),
                    cwd: PathBuf::from("2_Server"),
                    ready_port: Some(6900),
                    restart: RestartPolicy::On,
                },
                ServerSpec {
                    key: "char".into(),
                    exe: "char-server.exe".into(),
                    label: "Char-Server".into(),
                    cwd: PathBuf::from("2_Server"),
                    ready_port: Some(6121),
                    restart: RestartPolicy::On,
                },
                ServerSpec {
                    key: "web".into(),
                    exe: "web-server.exe".into(),
                    label: "Web-Server".into(),
                    cwd: PathBuf::from("2_Server"),
                    ready_port: Some(8888),
                    restart: RestartPolicy::On,
                },
                ServerSpec {
                    key: "map".into(),
                    exe: "map-server.exe".into(),
                    label: "Map-Server".into(),
                    cwd: PathBuf::from("2_Server"),
                    ready_port: Some(5121),
                    restart: RestartPolicy::On,
                },
            ],
            client: ClientSpec::default(),
        }
    }
}

/// Everything the launcher needs, with every path already absolute.
pub struct Resolved {
    pub root: PathBuf,
    pub cfg: Config,
}

impl Resolved {
    pub fn server_exe(&self, s: &ServerSpec) -> PathBuf {
        join(&self.root, &s.cwd).join(&s.exe)
    }

    pub fn server_cwd(&self, s: &ServerSpec) -> PathBuf {
        join(&self.root, &s.cwd)
    }

    pub fn laragon_exe(&self) -> PathBuf {
        join(&self.root, &self.cfg.laragon.exe)
    }

    pub fn mysql_exe(&self) -> PathBuf {
        join(&self.root, &self.cfg.laragon.mysql_exe)
    }

    pub fn mysql_admin_exe(&self) -> PathBuf {
        join(&self.root, &self.cfg.laragon.mysql_admin_exe)
    }

    pub fn mysql_ini(&self) -> PathBuf {
        join(&self.root, &self.cfg.laragon.mysql_ini)
    }

    pub fn client_exe(&self) -> PathBuf {
        join(&self.root, &self.cfg.client.exe)
    }

    pub fn client_cwd(&self) -> PathBuf {
        join(&self.root, &self.cfg.client.cwd)
    }

    pub fn log_file(&self) -> PathBuf {
        join(&self.root, &self.cfg.log_file)
    }

    pub fn mariadb_dir(&self) -> PathBuf {
        join(&self.root, &self.cfg.mariadb_tail.dir)
    }
}

pub fn join(root: &Path, rel: &Path) -> PathBuf {
    if rel.is_absolute() {
        rel.to_path_buf()
    } else {
        root.join(rel)
    }
}

/// A directory is the project root if the Laragon executable lives in it.
pub fn is_project_root(dir: &Path) -> bool {
    dir.join("1_Database").join("laragon.exe").is_file()
}

/// Find the project root and, if present next to it, `ro_launcher.toml`.
pub fn discover(cli_root: Option<&Path>) -> (PathBuf, Option<Config>) {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(r) = cli_root {
        candidates.push(r.to_path_buf());
    }
    if let Ok(env_root) = std::env::var("RO_ROOT") {
        candidates.push(PathBuf::from(env_root));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // Handles both "exe placed in the root folder" and `cargo run`
            // (target/debug -> target -> crate dir -> root).
            for up in [0usize, 1, 2, 3] {
                candidates.push(dir.ancestors().nth(up).unwrap_or(dir).to_path_buf());
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.clone());
        candidates.extend(cwd.ancestors().skip(1).take(4).map(|p| p.to_path_buf()));
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."));

    for cand in &candidates {
        let cfg_path = cand.join("ro_launcher.toml");
        if cfg_path.is_file() {
            match Config::from_file(&cfg_path) {
                Ok(cfg) => return (normalize(cand), Some(cfg)),
                Err(e) => {
                    eprintln!("ro_launcher: ignoring {}: {e:#}", cfg_path.display());
                }
            }
        }
    }
    for cand in &candidates {
        if is_project_root(cand) {
            return (normalize(cand), None);
        }
    }
    (
        normalize(&std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
        None,
    )
}

fn normalize(p: &Path) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = c.to_string_lossy().to_string();
    // canonicalize() returns a verbatim `\\?\` path; strip it so the value can
    // be written into Laragon's/MySQL's config files verbatim.
    let cleaned = s
        .strip_prefix(r"\\?\UNC\")
        .map(|r| format!(r"\\{r}"))
        .or_else(|| s.strip_prefix(r"\\?\").map(|r| r.to_string()))
        .unwrap_or(s);
    PathBuf::from(cleaned)
}

impl Config {
    pub fn from_file(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let cfg: Config = toml::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        Ok(cfg)
    }
}