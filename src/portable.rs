use crate::logging::Logger;
use std::path::{Path, PathBuf};

/// Laragon rewrites absolute paths into its own config files. When the whole
/// tree is copied to a different drive those paths keep pointing at the old
/// location, MariaDB refuses to start and the stack silently breaks.
/// The launcher always repairs this on startup so the install stays portable.
const STALE_SEGMENT: &str = "1_database";
const TEXT_EXTS: [&str; 9] = ["ini", "conf", "cnf", "cmd", "bat", "manifest", "env", "txt", "cfg"];
const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_DEPTH: usize = 8;

pub struct FixReport {
    pub rewritten: Vec<(PathBuf, usize)>,
    pub scanned: usize,
}

pub fn repair_stale_paths(root: &Path, dirs: &[PathBuf], backup_suffix: &str, log: &Logger) -> FixReport {
    let mut report = FixReport {
        rewritten: Vec::new(),
        scanned: 0,
    };
    for rel in dirs {
        let dir = crate::config::join(root, rel);
        if !dir.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        collect(&dir, 0, &mut files);
        for file in files {
            report.scanned += 1;
            match repair_file(root, &file, backup_suffix) {
                Ok(Some(count)) => report.rewritten.push((file, count)),
                Ok(None) => {}
                Err(e) => log.warn(format!(
                    "{}: could not rewrite stale paths ({e:#})",
                    file.display()
                )),
            }
        }
    }
    report
}

/// Public wrapper used by the dry-run reporter.
pub fn collect_public(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    collect(dir, depth, out)
}

/// Public wrapper used by the dry-run reporter.
pub fn rewrite_public(text: &str, root: &Path) -> (String, usize) {
    rewrite(text, root)
}

/// Creates (or reuses) a space-free directory junction pointing at `root`.
///
/// Laragon refuses to run when its own path contains a space, so when the stack
/// lives somewhere like `D:\My Games\RO_PreRenewal` we hand Laragon a junction
/// such as `D:\RO_Link` instead. Junctions need no administrator rights, and
/// every path Laragon stores still resolves through the link.
pub fn space_free_link(root: &Path, link: &Path, log: &Logger) -> anyhow::Result<PathBuf> {
    let root_str = root.to_string_lossy().to_string();
    if !root_str.contains(' ') {
        return Ok(root.to_path_buf());
    }
    let link_str = link.to_string_lossy().to_string();
    if link_str.contains(' ') {
        anyhow::bail!("junction path '{link_str}' must not contain a space");
    }
    if let Ok(existing) = std::fs::canonicalize(link) {
        if existing == std::fs::canonicalize(root).unwrap_or(root.to_path_buf()) {
            return Ok(link.to_path_buf());
        }
    }
    if link.exists() {
        // Something else is squatting there.
        let _ = std::fs::remove_file(link);
        let _ = std::fs::remove_dir_all(link);
    }
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let out = std::process::Command::new("cmd")
        .args([
            "/C",
            "mklink",
            "/J",
            &link_str,
            &root_str,
        ])
        .creation_flags_no_window()
        .output()?;
    let ok = link.exists();
    let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !ok {
        anyhow::bail!("mklink /J failed ({}): {msg}", out.status);
    }
    log.step(format!("Junction {link_str} -> {root_str}"));
    Ok(link.to_path_buf())
}

trait NoWindow {
    fn creation_flags_no_window(&mut self) -> &mut Self;
}

#[cfg(windows)]
impl NoWindow for std::process::Command {
    fn creation_flags_no_window(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.creation_flags(0x0800_0000)
    }
}

#[cfg(not(windows))]
impl NoWindow for std::process::Command {
    fn creation_flags_no_window(&mut self) -> &mut Self {
        self
    }
}

fn collect(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            collect(&path, depth + 1, out);
        } else if ft.is_file() {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if TEXT_EXTS.contains(&ext.as_str()) {
                out.push(path);
            }
        }
    }
}

/// Returns the number of replacements, or `None` when nothing changed.
fn repair_file(root: &Path, file: &Path, backup_suffix: &str) -> anyhow::Result<Option<usize>> {
    let meta = std::fs::metadata(file)?;
    if meta.len() > MAX_FILE_BYTES {
        return Ok(None);
    }
    let bytes = std::fs::read(file)?;
    // latin1 (byte <-> char identity) keeps every byte intact on write-back.
    let original: String = bytes.iter().map(|b| *b as char).collect();
    let (updated, count) = rewrite(&original, root);
    if count == 0 {
        return Ok(None);
    }
    let bak = PathBuf::from(format!("{}{}", file.display(), backup_suffix));
    if !bak.exists() {
        std::fs::copy(file, &bak)?;
    }
    std::fs::write(file, updated.as_bytes())?;
    Ok(Some(count))
}

/// Replaces every stale absolute path that points at the Laragon root with the
/// current root, preserving the separator style used by the file.
///
/// `G:\RO_PreRenewal\1_Database\data` + root `D:\NewGaems\Ragnarok Offline\RO_PreRenewal`
/// becomes `D:\NewGaems\Ragnarok Offline\RO_PreRenewal\1_Database\data`, because
/// everything up to and including the root folder *name* is swapped.
fn rewrite(text: &str, root: &Path) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    let root_norm = normalize_root(root);
    let root_name = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    let mut count = 0usize;
    while i < chars.len() {
        let start = i;
        let is_drive = i + 2 < chars.len()
            && chars[i].is_ascii_alphabetic()
            && chars[i + 1] == ':'
            && (chars[i + 2] == '\\' || chars[i + 2] == '/');
        if !is_drive {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let mut end = i;
        while end < chars.len() && is_path_char(chars[end]) {
            end += 1;
        }
        let matched: String = chars[start..end].iter().collect();
        let lowered = matched.to_ascii_lowercase().replace('/', "\\");
        let points_at_laragon = lowered.contains(&format!("\\{STALE_SEGMENT}"))
            || lowered.ends_with(STALE_SEGMENT);
        let already_current = lowered.starts_with(&root_norm);

        if points_at_laragon && !already_current {
            let sep = if matched.contains('\\') { '\\' } else { '/' };
            // Anchor on the root folder name when it survives the move, else on
            // the `1_Database` segment itself.
            // Anchor on the root folder name when it survived the move (then the
            // tail starts after it), otherwise on `1_Database` (tail keeps it).
            let anchor = if root_name.is_empty() {
                None
            } else {
                rfind_segment(&lowered, &root_name).map(|(_, after)| after)
            }
            .or_else(|| rfind_segment(&lowered, STALE_SEGMENT).map(|(start, _)| start));
            let tail = match anchor {
                Some(idx) => matched[idx..].replace('/', &sep.to_string()),
                None => matched[3..].replace('/', &sep.to_string()),
            };
            out.push_str(root.to_string_lossy().trim_end_matches(['\\', '/']));
            out.push_str(&tail);
            count += 1;
        } else {
            out.push_str(&matched);
        }
        i = end.max(start + 1);
    }
    (out, count)
}

/// Byte offset *just past* the last whole-segment occurrence of `segment` in a
/// lowercased, backslash-normalized path - i.e. where the tail should start.
/// Both strings have identical byte layout (lowercase + `/`->`\` are ASCII-1:1),
/// so the offset is valid for the original text too.
fn rfind_segment(lowered: &str, segment: &str) -> Option<(usize, usize)> {
    let needle = format!("\\{segment}");
    let mut best = None;
    let mut from = 0usize;
    while let Some(pos) = lowered[from..].find(&needle) {
        let at = from + pos;
        let after = at + needle.len();
        let is_end = after == lowered.len();
        let is_seg = after < lowered.len() && lowered.as_bytes()[after] == b'\\';
        if is_end || is_seg {
            best = Some((at, after));
        }
        from = at + 1;
        if from >= lowered.len() {
            break;
        }
    }
    best
}

fn is_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '\\' | '/' | '.' | '-' | '_' | ':')
}

fn normalize_root(root: &Path) -> String {
    root.display()
        .to_string()
        .to_ascii_lowercase()
        .replace('/', "\\")
}

#[cfg(test)]
mod tests {
    use super::rewrite;
    use std::path::Path;

    #[test]
    fn rewrites_stale_drive_paths() {
        let root = Path::new(r"D:\NewGaems\Ragnarok Offline\RO_PreRenewal");
        let input = "datadir= \"G:/RO_PreRenewal/1_Database/data/mariadb-11.8\"\r\nDataDir=G:\\RO_PreRenewal\\1_Database\\data\r\n";
        let (out, n) = rewrite(input, root);
        assert_eq!(n, 2);
        assert!(
            out.contains(r"D:\NewGaems\Ragnarok Offline\RO_PreRenewal\1_Database\data"),
            "got: {out}"
        );
        assert!(out.contains("D:\\NewGaems\\Ragnarok Offline\\RO_PreRenewal\\1_Database\\data"), "got: {out}");
    }

    #[test]
    fn rewrites_when_the_root_folder_was_renamed() {
        let root = Path::new(r"D:\Games\RO");
        let (out, n) = rewrite("datadir= \"G:\\RO_PreRenewal\\1_Database\\data\"\r\n", root);
        assert_eq!(n, 1);
        assert!(out.contains("datadir= \"D:\\Games\\RO\\1_Database\\data\""), "got: {out}");
    }

    #[test]
    fn keeps_current_paths_untouched() {
        let root = Path::new(r"D:\NewGaems\RO_PreRenewal");
        let input = "DataDir=D:\\NewGaems\\RO_PreRenewal\\1_Database\\data\r\n";
        let (out, n) = rewrite(input, root);
        assert_eq!(n, 0);
        assert_eq!(out, input);
    }

    #[test]
    fn ignores_unrelated_paths() {
        let root = Path::new(r"D:\NewGaems\RO");
        let (out, n) = rewrite("PATH=C:\\Windows\\System32;%PATH%\r\n", root);
        assert_eq!(n, 0);
        assert_eq!(out, "PATH=C:\\Windows\\System32;%PATH%\r\n");
    }
}