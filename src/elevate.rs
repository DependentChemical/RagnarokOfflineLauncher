//! Self-elevation.
//!
//! `3_Client/Ragnarok.exe` carries a `requireAdministrator` manifest, so
//! spawning it from a medium-integrity launcher fails with
//! `ERROR_ELEVATION_REQUIRED (740)`. Rather than guessing, the launcher asks
//! for elevation once, up front, and re-launches itself through UAC.

#[cfg(windows)]
use anyhow::Result;

#[cfg(windows)]
pub fn is_elevated() -> bool {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut size = std::mem::size_of::<TOKEN_ELEVATION>() as u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut TOKEN_ELEVATION as *mut _ as *mut std::ffi::c_void),
            size,
            &mut size,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    true
}

/// Re-launches this executable with the `runas` verb, forwarding all arguments.
/// Blocks until Windows reports the request was accepted (the UAC prompt itself
/// is shown by the shell, so this can wait for the user to click Yes).
#[cfg(windows)]
pub fn relaunch_elevated() -> Result<()> {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let exe = std::env::current_exe()?;
    let exe_w = HSTRING::from(exe.as_os_str());
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args_w = HSTRING::from(args.join(" "));

    unsafe {
        let result = ShellExecuteW(
            None,
            &HSTRING::from("runas"),
            PCWSTR(exe_w.as_ptr()),
            PCWSTR(args_w.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        // ShellExecuteW reports failure through GetLastError, not the result.
        if result.0 as isize <= 32 {
            return Err(anyhow::anyhow!(
                "elevation was declined or failed (ShellExecuteW = {})",
                result.0 as isize
            ));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn relaunch_elevated() -> Result<()> {
    anyhow::bail!("elevation is only implemented on Windows")
}