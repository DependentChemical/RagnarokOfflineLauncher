use anyhow::{anyhow, Context, Result};
use rand::Rng;
use std::fs;
use std::os::windows::fs::symlink_dir;
use std::path::{Path, PathBuf};
use std::process;

/// Creates or reuses a space-free directory junction for the given root.
/// Returns the symlink path that should be used as the effective root.
pub fn ensure_symlink(real_root: &Path, log: &crate::logging::Logger) -> Result<PathBuf> {
    let exe_dir = exe_directory()?;
    let folderpath_file = exe_dir.join("folderpath");

    let drive_root = real_root
        .components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .ok_or_else(|| anyhow!("cannot determine drive root of {}", real_root.display()))?;

    let base_name = "RO_OFFLINE";
    let mut link_path = PathBuf::from(drive_root).join(base_name);

    if link_path.exists() {
        if let Ok(existing_target) = fs::read_link(&link_path) {
            let canon_existing = fs::canonicalize(&existing_target).unwrap_or(existing_target);
            let canon_real = fs::canonicalize(real_root).unwrap_or_else(|_| real_root.to_path_buf());
            if canon_existing == canon_real {
                log.step(format!("Reusing existing junction: {}", link_path.display()));
                acquire_lock(&link_path, &exe_dir)?;
                write_folderpath(&folderpath_file, &link_path)?;
                return Ok(link_path);
            }
        }
        link_path = find_available_link(&link_path, drive_root)?;
    }

    if let Some(parent) = link_path.parent() {
        fs::create_dir_all(parent).context("creating parent for junction")?;
    }

    log.step(format!(
        "Creating junction {} -> {}",
        link_path.display(),
        real_root.display()
    ));

    symlink_dir(real_root, &link_path)
        .with_context(|| format!("creating junction at {}", link_path.display()))?;

    write_folderpath(&folderpath_file, &link_path)?;
    acquire_lock(&link_path, &exe_dir)?;

    Ok(link_path)
}

/// Finds an available symlink name by appending _XXX suffix.
fn find_available_link(_original: &Path, drive_root: &str) -> Result<PathBuf> {
    let mut rng = rand::rng();
    for _ in 0..100 {
        let suffix: String = (0..3)
            .map(|_| rng.sample(rand::distr::Alphanumeric) as char)
            .collect();
        let candidate = PathBuf::from(drive_root).join(format!("RO_OFFLINE_{}", suffix));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    anyhow::bail!("could not find available RO_OFFLINE_XXX name after 100 attempts")
}

/// Acquires a lock file for this launcher instance.
pub fn acquire_lock(_link_path: &Path, exe_dir: &Path) -> Result<PathBuf> {
    let pid = process::id();
    let folderpath_file = exe_dir.join("folderpath");
    let folderpath_str = fs::read_to_string(&folderpath_file).unwrap_or_default();
    let lock_name = format!("folderpath{}.lock", pid);
    let lock_path = exe_dir.join(lock_name);

    fs::write(&lock_path, &folderpath_str).context("writing lock file")?;
    Ok(lock_path)
}

/// Releases this launcher's lock file.
pub fn release_lock(lock_path: &Path) -> Result<()> {
    if lock_path.exists() {
        fs::remove_file(lock_path).context("removing lock file")?;
    }
    Ok(())
}

/// Checks if there are any other lock files for this symlink.
pub fn is_last_user(exe_dir: &Path) -> bool {
    let entries = fs::read_dir(exe_dir).ok();
    if let Some(entries) = entries {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("folderpath") && name.ends_with(".lock") {
                return false;
            }
        }
    }
    true
}

/// Cleans up the symlink and folderpath file if this is the last user.
pub fn cleanup_symlink(link_path: &Path, exe_dir: &Path, log: &crate::logging::Logger) {
    if !is_last_user(exe_dir) {
        log.step("Other launcher instances still running; keeping junction");
        return;
    }

    log.step(format!("Removing junction: {}", link_path.display()));
    let _ = fs::remove_dir_all(link_path);

    let folderpath_file = exe_dir.join("folderpath");
    if folderpath_file.exists() {
        let _ = fs::remove_file(&folderpath_file);
    }
    log.step("Junction and folderpath cleaned up");
}

fn write_folderpath(folderpath_file: &Path, link_path: &Path) -> Result<()> {
    fs::write(folderpath_file, link_path.to_string_lossy().as_bytes())
        .context("writing folderpath file")?;
    Ok(())
}

fn exe_directory() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("getting current exe path")?;
    exe.parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| anyhow!("exe has no parent directory"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_find_available_link() {
        let dir = tempdir().unwrap();
        let drive = dir.path().to_str().unwrap();
        let base = PathBuf::from(drive).join("RO_OFFLINE");
        fs::create_dir_all(&base).unwrap();

        let result = find_available_link(&base, drive).unwrap();
        assert!(result.to_string_lossy().contains("RO_OFFLINE_"));
        assert!(!result.exists());
    }
}