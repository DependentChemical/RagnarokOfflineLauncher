use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RestartPolicy {
    On,
    Off,
}

impl RestartPolicy {
    pub fn as_bool(self) -> bool {
        matches!(self, RestartPolicy::On)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LaragonSpec {
    pub exe: PathBuf,
    pub mysql_admin_exe: PathBuf,
    pub db_port: u16,
    pub db_host: String,
    pub ready_timeout_secs: u64,
    pub graceful_db_shutdown: bool,
    pub db_user: String,
    pub db_password: String,
    pub hide_window: bool,
}

impl Default for LaragonSpec {
    fn default() -> Self {
        Self {
            exe: PathBuf::from("1_Database/laragon.exe"),
            mysql_admin_exe: PathBuf::from(
                "1_Database/bin/mysql/mariadb-11.8.8-winx64/bin/mariadb-admin.exe",
            ),
            db_port: 3306,
            db_host: "127.0.0.1".into(),
            ready_timeout_secs: 90,
            graceful_db_shutdown: true,
            db_user: "root".into(),
            db_password: String::new(),
            hide_window: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerSpec {
    pub key: String,
    pub exe: String,
    pub label: String,
    pub cwd: PathBuf,
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
pub struct Config {
    pub laragon: LaragonSpec,
    pub servers: Vec<ServerSpec>,
    pub client: ClientSpec,
    pub restart_delay_secs: u64,
    pub server_ready_timeout_secs: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
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
            restart_delay_secs: 15,
            server_ready_timeout_secs: 30,
        }
    }
}

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

    pub fn mysql_admin_exe(&self) -> PathBuf {
        join(&self.root, &self.cfg.laragon.mysql_admin_exe)
    }

    pub fn client_exe(&self) -> PathBuf {
        join(&self.root, &self.cfg.client.exe)
    }

    pub fn client_cwd(&self) -> PathBuf {
        join(&self.root, &self.cfg.client.cwd)
    }
}

pub fn join(root: &Path, rel: &Path) -> PathBuf {
    if rel.is_absolute() {
        rel.to_path_buf()
    } else {
        root.join(rel)
    }
}

pub fn is_project_root(dir: &Path) -> bool {
    dir.join("1_Database").join("laragon.exe").is_file()
}

pub fn discover(cli_root: Option<&Path>) -> (PathBuf, Option<Config>) {
    if let Some(r) = cli_root {
        let normalized = normalize(r);
        let cfg_path = normalized.join("ro_launcher.toml");
        let cfg = if cfg_path.is_file() {
            Config::from_file(&cfg_path).ok()
        } else {
            None
        };
        return (normalized, cfg);
    }

    if let Ok(exe) = std::env::current_exe() {
        for anc in exe.ancestors() {
            if is_project_root(anc) {
                let normalized = normalize(anc);
                let cfg_path = normalized.join("ro_launcher.toml");
                let cfg = if cfg_path.is_file() {
                    Config::from_file(&cfg_path).ok()
                } else {
                    None
                };
                return (normalized, cfg);
            }
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        for anc in cwd.ancestors() {
            if is_project_root(anc) {
                let normalized = normalize(anc);
                let cfg_path = normalized.join("ro_launcher.toml");
                let cfg = if cfg_path.is_file() {
                    Config::from_file(&cfg_path).ok()
                } else {
                    None
                };
                return (normalized, cfg);
            }
        }
    }

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let normalized = normalize(&manifest);
    let cfg_path = normalized.join("ro_launcher.toml");
    let cfg = if cfg_path.is_file() {
        Config::from_file(&cfg_path).ok()
    } else {
        None
    };
    (normalized, cfg)
}

fn normalize(p: &Path) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = c.to_string_lossy().to_string();
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