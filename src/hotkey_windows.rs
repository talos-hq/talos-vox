//! Windows global hotkey through a low-level keyboard hook (`WH_KEYBOARD_LL`).
//!
//! The hook sees every key event in the user's session without focus, which
//! is what push-to-talk needs: `Pressed` on the first key-down (auto-repeat is
//! folded away), `Released` on key-up. Events pass through untouched; the hook
//! never swallows the key. It needs no privilege, but UIPI applies: while an
//! elevated window has focus, a non-elevated daemon receives nothing from it.

#![cfg_attr(not(windows), allow(dead_code))]

use crate::config::HotkeyConfig;
use crate::error::{HotkeyError, Result};
use tokio::sync::mpsc;

/// Same shape as the Linux and macOS events so the daemon's match arms stay
/// platform-agnostic. Model and profile overrides are not surfaced here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyEvent {
    Pressed {
        model_override: Option<String>,
        profile_override: Option<String>,
    },
    Released,
    Cancel,
}

pub trait HotkeyListener: Send {
    fn start(&mut self) -> Result<mpsc::Receiver<HotkeyEvent>>;
    fn stop(&mut self) -> Result<()>;
}

/// Virtual-key code for a hotkey name (the evdev-style names the config
/// already uses, plus the macOS aliases). Left/right modifiers are distinct:
/// the low-level hook reports `VK_LMENU`/`VK_RMENU`, never `VK_MENU`.
pub(crate) fn parse_key_name(name: &str) -> Option<u32> {
    let upper = name.to_uppercase();
    let upper = upper.strip_prefix("KEY_").unwrap_or(&upper);
    if let Some(n) = upper.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=24).contains(&n).then_some(0x70 + n - 1);
    }
    let bytes = upper.as_bytes();
    if bytes.len() == 1 && (bytes[0].is_ascii_uppercase() || bytes[0].is_ascii_digit()) {
        return Some(bytes[0] as u32);
    }
    Some(match upper {
        "LEFTALT" | "LEFTOPT" | "LEFTOPTION" | "ALT" | "OPTION" => 0xA4,
        "RIGHTALT" | "RIGHTOPT" | "RIGHTOPTION" | "ALTGR" => 0xA5,
        "LEFTCTRL" | "LEFTCONTROL" | "CTRL" | "CONTROL" => 0xA2,
        "RIGHTCTRL" | "RIGHTCONTROL" => 0xA3,
        "LEFTSHIFT" | "SHIFT" => 0xA0,
        "RIGHTSHIFT" => 0xA1,
        "LEFTMETA" | "LEFTWIN" | "LEFTCMD" | "META" | "WIN" | "SUPER" | "CMD" => 0x5B,
        "RIGHTMETA" | "RIGHTWIN" | "RIGHTCMD" => 0x5C,
        "ESCAPE" | "ESC" => 0x1B,
        "SPACE" => 0x20,
        "TAB" => 0x09,
        "CAPSLOCK" => 0x14,
        "BACKSPACE" => 0x08,
        "ENTER" | "RETURN" => 0x0D,
        "UP" | "UPARROW" => 0x26,
        "DOWN" | "DOWNARROW" => 0x28,
        "LEFT" | "LEFTARROW" => 0x25,
        "RIGHT" | "RIGHTARROW" => 0x27,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" => 0x21,
        "PAGEDOWN" => 0x22,
        "DELETE" => 0x2E,
        "INSERT" => 0x2D,
        "PAUSE" => 0x13,
        "SCROLLLOCK" => 0x91,
        "PRINTSCREEN" | "SYSRQ" => 0x2C,
        "MENU" | "COMPOSE" => 0x5D,
        _ => return None,
    })
}

/// What a key event means for push-to-talk. `held` tracks whether the hotkey
/// is down, so auto-repeat key-downs produce nothing.
pub(crate) fn translate(
    vk: u32,
    down: bool,
    target: u32,
    cancel: Option<u32>,
    held: &mut bool,
) -> Option<HotkeyEvent> {
    if vk == target {
        return match (down, *held) {
            (true, false) => {
                *held = true;
                Some(HotkeyEvent::Pressed {
                    model_override: None,
                    profile_override: None,
                })
            }
            (false, true) => {
                *held = false;
                Some(HotkeyEvent::Released)
            }
            _ => None,
        };
    }
    (down && Some(vk) == cancel).then_some(HotkeyEvent::Cancel)
}

pub fn create_listener(
    config: &HotkeyConfig,
    _secondary_model: Option<String>,
) -> Result<Box<dyn HotkeyListener>> {
    let target =
        parse_key_name(&config.key).ok_or_else(|| HotkeyError::UnknownKey(config.key.clone()))?;
    let cancel = config.cancel_key.as_deref().and_then(parse_key_name);
    Ok(Box::new(imp::HookListener::new(target, cancel)))
}

#[cfg(windows)]
mod imp {
    use super::{translate, HotkeyEvent, HotkeyListener};
    use crate::error::{HotkeyError, Result};
    use std::sync::Mutex;
    use tokio::sync::mpsc;
    use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
        HC_ACTION, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_QUIT,
        WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    struct HookState {
        target: u32,
        cancel: Option<u32>,
        held: bool,
        tx: mpsc::Sender<HotkeyEvent>,
    }

    /// The hook procedure takes no user data, so its state is process-global.
    /// One listener at a time; the daemon creates exactly one.
    static STATE: Mutex<Option<HookState>> = Mutex::new(None);

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let down = match wparam as u32 {
                WM_KEYDOWN | WM_SYSKEYDOWN => Some(true),
                WM_KEYUP | WM_SYSKEYUP => Some(false),
                _ => None,
            };
            if let Some(down) = down {
                // SAFETY: for WH_KEYBOARD_LL with HC_ACTION, lparam points
                // at a KBDLLHOOKSTRUCT valid for the duration of the call.
                let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
                if let Ok(mut guard) = STATE.lock() {
                    if let Some(state) = guard.as_mut() {
                        let (target, cancel) = (state.target, state.cancel);
                        if let Some(event) =
                            translate(info.vkCode, down, target, cancel, &mut state.held)
                        {
                            // Never block the hook: Windows unhooks a
                            // procedure that stalls past LowLevelHooksTimeout.
                            let _ = state.tx.try_send(event);
                        }
                    }
                }
            }
        }
        unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
    }

    pub struct HookListener {
        target: u32,
        cancel: Option<u32>,
        thread: Option<(u32, std::thread::JoinHandle<()>)>,
    }

    impl HookListener {
        pub fn new(target: u32, cancel: Option<u32>) -> Self {
            Self {
                target,
                cancel,
                thread: None,
            }
        }
    }

    impl HotkeyListener for HookListener {
        fn start(&mut self) -> Result<mpsc::Receiver<HotkeyEvent>> {
            let (tx, rx) = mpsc::channel(32);
            *STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(HookState {
                target: self.target,
                cancel: self.cancel,
                held: false,
                tx,
            });
            // The hook is serviced by the message loop of the thread that
            // installed it, so it gets a dedicated thread.
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let handle = std::thread::Builder::new()
                .name("voxtype-hotkey".into())
                .spawn(move || {
                    let thread_id = unsafe { GetCurrentThreadId() };
                    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
                    let hook =
                        unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), module, 0) };
                    if hook.is_null() {
                        let _ = ready_tx.send(Err(std::io::Error::last_os_error()));
                        return;
                    }
                    let _ = ready_tx.send(Ok(thread_id));
                    let mut msg: MSG = unsafe { std::mem::zeroed() };
                    while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {}
                    unsafe { UnhookWindowsHookEx(hook) };
                })
                .map_err(|e| HotkeyError::DeviceAccess(e.to_string()))?;
            match ready_rx.recv() {
                Ok(Ok(thread_id)) => {
                    self.thread = Some((thread_id, handle));
                    Ok(rx)
                }
                Ok(Err(e)) => Err(HotkeyError::DeviceAccess(format!(
                    "SetWindowsHookExW(WH_KEYBOARD_LL) failed: {e}"
                ))
                .into()),
                Err(_) => Err(HotkeyError::DeviceAccess("hotkey thread exited".into()).into()),
            }
        }

        fn stop(&mut self) -> Result<()> {
            if let Some((thread_id, handle)) = self.thread.take() {
                unsafe { PostThreadMessageW(thread_id, WM_QUIT, 0, 0) };
                let _ = handle.join();
            }
            *STATE.lock().unwrap_or_else(|e| e.into_inner()) = None;
            Ok(())
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{HotkeyEvent, HotkeyListener};
    use crate::error::{HotkeyError, Result};
    use tokio::sync::mpsc;

    pub struct HookListener;

    impl HookListener {
        pub fn new(_target: u32, _cancel: Option<u32>) -> Self {
            Self
        }
    }

    impl HotkeyListener for HookListener {
        fn start(&mut self) -> Result<mpsc::Receiver<HotkeyEvent>> {
            Err(HotkeyError::DeviceAccess("WH_KEYBOARD_LL is Windows-only".into()).into())
        }
        fn stop(&mut self) -> Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names_map_to_virtual_keys() {
        assert_eq!(parse_key_name("SCROLLLOCK"), Some(0x91));
        assert_eq!(parse_key_name("scrolllock"), Some(0x91));
        assert_eq!(parse_key_name("KEY_SCROLLLOCK"), Some(0x91));
        assert_eq!(parse_key_name("F1"), Some(0x70));
        assert_eq!(parse_key_name("F24"), Some(0x87));
        assert_eq!(parse_key_name("F25"), None);
        assert_eq!(parse_key_name("RIGHTALT"), Some(0xA5));
        assert_eq!(parse_key_name("rightoption"), Some(0xA5));
        assert_eq!(parse_key_name("LEFTMETA"), Some(0x5B));
        assert_eq!(parse_key_name("a"), Some(0x41));
        assert_eq!(parse_key_name("7"), Some(0x37));
        assert_eq!(parse_key_name("nonsense"), None);
    }

    #[test]
    fn auto_repeat_is_folded_into_one_press() {
        let mut held = false;
        let t = 0x91;
        let mut run = |vk, down| translate(vk, down, t, Some(0x1B), &mut held);
        assert!(matches!(run(t, true), Some(HotkeyEvent::Pressed { .. })));
        assert_eq!(run(t, true), None);
        assert_eq!(run(t, true), None);
        assert_eq!(run(0x41, true), None);
        assert_eq!(run(0x1B, true), Some(HotkeyEvent::Cancel));
        assert_eq!(run(0x1B, false), None);
        assert_eq!(run(t, false), Some(HotkeyEvent::Released));
        assert_eq!(run(t, false), None);
    }

    /// On the Windows CI runner: the hook installs, sees an injected key
    /// press and release, and uninstalls.
    #[cfg(windows)]
    #[test]
    fn low_level_hook_sees_injected_keys() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
        };
        let config = HotkeyConfig {
            key: "F24".into(),
            ..HotkeyConfig::default()
        };
        let mut listener = create_listener(&config, None).unwrap();
        let mut rx = listener.start().expect("hook installs");
        let key = |flags| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: 0x87,
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let inputs = [key(0), key(KEYEVENTF_KEYUP)];
        let sent = unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        };
        assert_eq!(sent, 2, "SendInput injected both events");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut seen = Vec::new();
        while seen.len() < 2 && std::time::Instant::now() < deadline {
            match rx.try_recv() {
                Ok(e) => seen.push(e),
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
        listener.stop().unwrap();
        assert!(
            matches!(seen.first(), Some(HotkeyEvent::Pressed { .. })),
            "{seen:?}"
        );
        assert_eq!(seen.get(1), Some(&HotkeyEvent::Released));
    }
}
