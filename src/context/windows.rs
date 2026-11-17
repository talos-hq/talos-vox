//! Windows: the foreground window's process image name and title.
//!
//! `GetForegroundWindow` -> `GetWindowThreadProcessId` -> `OpenProcess`
//! (limited query rights, so it works for most processes without
//! elevation) -> `QueryFullProcessImageNameW`. The app id is the exe stem,
//! lowercased (`code`, `firefox`, `windowsterminal`), the same identity
//! Hex's Windows modes match on. Elevated processes may refuse the query;
//! then there is no snapshot.

use super::{AppContext, FocusProbe};

pub(super) struct Windows;

impl FocusProbe for Windows {
    fn name(&self) -> &'static str {
        "windows"
    }

    fn snapshot(&self) -> Option<AppContext> {
        imp::snapshot()
    }
}

/// `C:\Program Files\Microsoft VS Code\Code.exe` -> `code`.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn app_id_from_image(path: &str) -> Option<String> {
    let file = path.rsplit(['\\', '/']).next()?;
    let stem = match file.rsplit_once('.') {
        Some((stem, ext)) if ext.eq_ignore_ascii_case("exe") => stem,
        _ => file,
    };
    (!stem.is_empty()).then(|| stem.to_lowercase())
}

#[cfg(windows)]
mod imp {
    use super::{app_id_from_image, AppContext};
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
    };

    pub(super) fn snapshot() -> Option<AppContext> {
        // SAFETY: plain Win32 queries on handles we obtain here; every
        // buffer is sized and lengths are bounded by what the API wrote.
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() {
                return None;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == 0 {
                return None;
            }
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return None;
            }
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok =
                QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
            CloseHandle(process);
            if ok == 0 {
                return None;
            }
            let image = String::from_utf16_lossy(&buf[..len as usize]);

            let mut title = [0u16; 512];
            let n = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
            let title = (n > 0).then(|| String::from_utf16_lossy(&title[..n as usize]));

            Some(AppContext {
                app_id: app_id_from_image(&image)?,
                title,
                source: "windows",
            })
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::AppContext;

    pub(super) fn snapshot() -> Option<AppContext> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_stem_lowercased() {
        assert_eq!(
            app_id_from_image(r"C:\Program Files\Microsoft VS Code\Code.exe"),
            Some("code".into())
        );
        assert_eq!(
            app_id_from_image(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.EXE"),
            Some("powershell".into())
        );
        assert_eq!(app_id_from_image("noext"), Some("noext".into()));
        assert_eq!(app_id_from_image(r"C:\dir\"), None);
    }

    /// Runs on the Windows CI runner: the probe is picked and answers
    /// without crashing (a service session may have no foreground window).
    #[cfg(windows)]
    #[test]
    fn windows_probe_runs() {
        let probe = super::super::detect().expect("Windows always has a probe");
        assert_eq!(probe.name(), "windows");
        if let Some(app) = probe.snapshot() {
            assert!(!app.app_id.is_empty());
        }
    }
}
