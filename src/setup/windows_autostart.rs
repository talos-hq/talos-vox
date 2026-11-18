//! Start the daemon at login on Windows through the per-user Run key,
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`. Needs no elevation
//! and is what Settings > Apps > Startup lists and lets the user toggle.
//! Hex uses the same key.

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "voxtype";

/// The command stored in the Run value: the quoted exe path, then `daemon`.
pub(crate) fn run_command(exe: &std::path::Path) -> String {
    format!("\"{}\" daemon", exe.display())
}

#[cfg(windows)]
mod reg {
    use super::RUN_KEY;
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn err(code: u32) -> std::io::Error {
        std::io::Error::from_raw_os_error(code as i32)
    }

    pub fn set(name: &str, value: &str) -> std::io::Result<()> {
        let data = wide(value);
        let rc = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                wide(RUN_KEY).as_ptr(),
                wide(name).as_ptr(),
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            )
        };
        if rc == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(err(rc))
        }
    }

    pub fn get(name: &str) -> std::io::Result<Option<String>> {
        let (key, name) = (wide(RUN_KEY), wide(name));
        let mut len: u32 = 0;
        let rc = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut len,
            )
        };
        if rc == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if rc != ERROR_SUCCESS {
            return Err(err(rc));
        }
        let mut buf = vec![0u16; (len as usize).div_ceil(2)];
        let rc = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(err(rc));
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Ok(Some(String::from_utf16_lossy(&buf[..end])))
    }

    pub fn delete(name: &str) -> std::io::Result<bool> {
        let rc = unsafe {
            RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                wide(RUN_KEY).as_ptr(),
                wide(name).as_ptr(),
            )
        };
        match rc {
            ERROR_SUCCESS => Ok(true),
            ERROR_FILE_NOT_FOUND => Ok(false),
            rc => Err(err(rc)),
        }
    }
}

#[cfg(not(windows))]
mod reg {
    fn unsupported<T>() -> std::io::Result<T> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the Run key exists only on Windows (Linux: voxtype setup systemd; macOS: voxtype setup app-bundle)",
        ))
    }
    pub fn set(_: &str, _: &str) -> std::io::Result<()> {
        unsupported()
    }
    pub fn get(_: &str) -> std::io::Result<Option<String>> {
        unsupported()
    }
    pub fn delete(_: &str) -> std::io::Result<bool> {
        unsupported()
    }
}

pub async fn install() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let command = run_command(&exe);
    reg::set(VALUE_NAME, &command)?;
    println!("voxtype will start at login: HKCU\\{RUN_KEY}\\{VALUE_NAME} = {command}");
    println!(
        "Turn it off with: voxtype setup autostart --uninstall (or Settings > Apps > Startup)"
    );
    Ok(())
}

pub async fn uninstall() -> anyhow::Result<()> {
    if reg::delete(VALUE_NAME)? {
        println!("Removed the login item.");
    } else {
        println!("No login item was installed.");
    }
    Ok(())
}

pub async fn status() -> anyhow::Result<()> {
    match reg::get(VALUE_NAME)? {
        Some(command) => {
            println!("Login item installed: {command}");
            let current = run_command(&std::env::current_exe()?);
            if command != current {
                println!(
                    "It points at a different binary; reinstall with: voxtype setup autostart"
                );
            }
        }
        None => println!("No login item. Install with: voxtype setup autostart"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_command_quotes_the_exe_path() {
        let exe = std::path::Path::new(r"C:\Program Files\voxtype\voxtype.exe");
        assert_eq!(
            run_command(exe),
            r#""C:\Program Files\voxtype\voxtype.exe" daemon"#
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn other_platforms_refuse() {
        assert!(reg::get(VALUE_NAME).is_err());
    }

    /// On the Windows CI runner: a real Run-key round trip under a test name,
    /// so the real `voxtype` value is never touched.
    #[cfg(windows)]
    #[test]
    fn windows_run_key_round_trip() {
        let name = "voxtype-ci-test";
        let _ = reg::delete(name);
        assert_eq!(reg::get(name).unwrap(), None);
        reg::set(name, r#""C:\x y\voxtype.exe" daemon"#).unwrap();
        assert_eq!(
            reg::get(name).unwrap().as_deref(),
            Some(r#""C:\x y\voxtype.exe" daemon"#)
        );
        assert!(reg::delete(name).unwrap());
        assert!(!reg::delete(name).unwrap());
        assert_eq!(reg::get(name).unwrap(), None);
    }
}
