//! Client registry bootstrap.
//!
//! `3_Client\Ragnarok.exe` refuses to create its window unless
//! `HKCU\Software\gravity\RagnarokOnline` is fully populated. Running
//! `opensetup.exe` once writes those values, but a fresh (or moved) install
//! does not have them - the client then starts, spins, and never shows a
//! window. The launcher writes them itself so the stack stays portable with no
//! manual setup step, and only ever adds or refreshes those three paths; every
//! other client setting is left untouched.

use crate::logging::Logger;
use std::path::Path;

const KEY_PATH: &str = r"Software\gravity\RagnarokOnline";

/// `""` is the key's default value; the other two are read by the client.
const VALUES: [&str; 3] = ["", "RagPath", "SakrayPath"];

#[cfg(windows)]
use windows::Win32::Foundation::ERROR_SUCCESS;
#[cfg(windows)]
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE,
};

#[cfg(windows)]
pub fn ensure_client_registry(client_dir: &Path, log: &Logger) -> anyhow::Result<()> {
    use anyhow::anyhow;
    use windows::core::HSTRING;

    // Trailing separator: this is how opensetup.exe stores the paths.
    let mut dir = client_dir.to_string_lossy().to_string();
    if !dir.ends_with('\\') {
        dir.push('\\');
    }

    let mut key = HKEY::default();
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(KEY_PATH),
            None,
            None::<&windows::core::PCWSTR>,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(anyhow!("RegCreateKeyExW({KEY_PATH}) failed: {}", rc.0));
    }
    let result = unsafe { write_values(key, &dir, log) };
    unsafe {
        let _ = RegCloseKey(key);
    }
    result
}

#[cfg(windows)]
unsafe fn write_values(key: HKEY, dir: &str, log: &Logger) -> anyhow::Result<()> {
    use windows::core::HSTRING;

    let bytes: Vec<u8> = dir
        .encode_utf16()
        .chain(std::iter::once(0u16))
        .flat_map(u16::to_le_bytes)
        .collect();

    for name in VALUES {
        if current_value(key, name).as_deref() == Some(dir) {
            continue; // already correct - never touch the user's settings
        }
        // Drop a stale pointer left behind by a previous install location.
        let _ = RegDeleteValueW(key, &HSTRING::from(name));
        let rc = RegSetValueExW(key, &HSTRING::from(name), None, REG_SZ, Some(bytes.as_slice()));
        if rc == ERROR_SUCCESS {
            log.step(format!(
                "Client registry: HKCU\\{KEY_PATH}\\{} = {dir}",
                if name.is_empty() { "(Default)" } else { name }
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
unsafe fn current_value(key: HKEY, name: &str) -> Option<String> {
    use windows::core::HSTRING;

    let mut buf = vec![0u8; 2048];
    let mut size = buf.len() as u32;
    let mut kind = REG_VALUE_TYPE(0);
    let rc = RegQueryValueExW(
        key,
        &HSTRING::from(name),
        None,
        Some(&mut kind),
        Some(buf.as_mut_ptr()),
        Some(&mut size),
    );
    if rc != ERROR_SUCCESS {
        return None;
    }
    let utf16: Vec<u16> = buf[..(size as usize).min(buf.len())]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|c| *c != 0)
        .collect();
    String::from_utf16(&utf16).ok()
}

#[cfg(not(windows))]
pub fn ensure_client_registry(_client_dir: &Path, _log: &Logger) -> anyhow::Result<()> {
    Ok(())
}