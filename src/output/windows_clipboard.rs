//! Windows clipboard and paste output (`arboard` + `SendInput`).
//!
//! `clipboard` mode copies the text. `paste` mode copies it, sends the
//! configured paste chord (default Ctrl+V) with `SendInput`, and can put the
//! previous clipboard text back afterwards (`restore_clipboard`). Paste is the
//! answer for apps that drop `KEYEVENTF_UNICODE` input; it obeys the same
//! UIPI rule as typing (no input into elevated windows from a non-elevated
//! daemon).

#![cfg_attr(not(windows), allow(dead_code))]

use super::TextOutput;
use crate::error::OutputError;
use std::time::Duration;

/// Virtual keys for a `paste_keys` chord such as `ctrl+v` or `shift+insert`:
/// the modifiers in order, then the key.
pub(crate) fn parse_chord(spec: &str) -> Option<Vec<u16>> {
    let parts: Vec<String> = spec.split('+').map(|p| p.trim().to_lowercase()).collect();
    let (key, mods) = parts.split_last()?;
    let mut keys = Vec::with_capacity(parts.len());
    for m in mods {
        keys.push(match m.as_str() {
            "ctrl" | "control" => 0x11,
            "shift" => 0x10,
            "alt" => 0x12,
            "super" | "win" | "meta" | "logo" | "cmd" => 0x5B,
            _ => return None,
        });
    }
    keys.push(crate::hotkey_windows::parse_key_name(key)? as u16);
    Some(keys)
}

fn full_text(text: &str, append: &Option<String>) -> String {
    let mut s = text.to_string();
    if let Some(a) = append {
        s.push_str(a);
    }
    s
}

fn failed(what: &str, e: impl std::fmt::Display) -> OutputError {
    OutputError::InjectionFailed(format!("{what}: {e}"))
}

#[cfg(windows)]
fn set_clipboard(text: String) -> Result<(), OutputError> {
    arboard::Clipboard::new()
        .and_then(|mut c| c.set_text(text))
        .map_err(|e| failed("clipboard", e))
}

#[cfg(windows)]
fn get_clipboard() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

/// Press the keys in order, release them in reverse, as one `SendInput`
/// batch so no other input can interleave.
#[cfg(windows)]
fn send_chord(keys: &[u16]) -> Result<(), OutputError> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    };
    let event = |vk: u16, flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs: Vec<INPUT> = keys
        .iter()
        .map(|&k| event(k, 0))
        .chain(keys.iter().rev().map(|&k| event(k, KEYEVENTF_KEYUP)))
        .collect();
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        Err(failed(
            "SendInput",
            "input was blocked (UIPI: is the focused app elevated?)",
        ))
    }
}

pub struct WinClipboardOutput {
    append_text: Option<String>,
}

impl WinClipboardOutput {
    pub fn new(append_text: Option<String>) -> Self {
        Self { append_text }
    }
}

#[async_trait::async_trait]
impl TextOutput for WinClipboardOutput {
    async fn output(&self, text: &str) -> Result<(), OutputError> {
        #[cfg(windows)]
        {
            let text = full_text(text, &self.append_text);
            tokio::task::spawn_blocking(move || set_clipboard(text))
                .await
                .map_err(|e| failed("clipboard", e))?
        }
        #[cfg(not(windows))]
        {
            let _ = text;
            Err(failed("clipboard", "Windows only"))
        }
    }

    async fn is_available(&self) -> bool {
        cfg!(windows)
    }

    fn name(&self) -> &'static str {
        "windows-clipboard"
    }
}

pub struct WinPasteOutput {
    append_text: Option<String>,
    auto_submit: bool,
    chord: Vec<u16>,
    pre_paste_delay: Duration,
    restore: Option<Duration>,
}

impl WinPasteOutput {
    pub fn new(
        auto_submit: bool,
        append_text: Option<String>,
        paste_keys: Option<String>,
        pre_type_delay_ms: u32,
        restore_clipboard: bool,
        restore_clipboard_delay_ms: u32,
    ) -> Self {
        let spec = paste_keys.as_deref().unwrap_or("ctrl+v");
        let chord = parse_chord(spec).unwrap_or_else(|| {
            tracing::warn!("Invalid paste_keys '{spec}', using ctrl+v");
            vec![0x11, 0x56]
        });
        Self {
            append_text,
            auto_submit,
            chord,
            pre_paste_delay: Duration::from_millis(pre_type_delay_ms as u64),
            restore: restore_clipboard
                .then(|| Duration::from_millis(restore_clipboard_delay_ms as u64)),
        }
    }
}

#[async_trait::async_trait]
impl TextOutput for WinPasteOutput {
    async fn output(&self, text: &str) -> Result<(), OutputError> {
        #[cfg(windows)]
        {
            let text = full_text(text, &self.append_text);
            let previous = if self.restore.is_some() {
                tokio::task::spawn_blocking(get_clipboard)
                    .await
                    .ok()
                    .flatten()
            } else {
                None
            };
            tokio::task::spawn_blocking(move || set_clipboard(text))
                .await
                .map_err(|e| failed("clipboard", e))??;
            tokio::time::sleep(self.pre_paste_delay).await;
            send_chord(&self.chord)?;
            if self.auto_submit {
                send_chord(&[0x0D])?;
            }
            if let (Some(delay), Some(previous)) = (self.restore, previous) {
                // The target reads the clipboard asynchronously; restoring
                // too early pastes the old text instead.
                tokio::time::sleep(delay).await;
                let _ = tokio::task::spawn_blocking(move || set_clipboard(previous)).await;
            }
            Ok(())
        }
        #[cfg(not(windows))]
        {
            let _ = text;
            Err(failed("paste", "Windows only"))
        }
    }

    async fn is_available(&self) -> bool {
        cfg!(windows)
    }

    fn name(&self) -> &'static str {
        "windows-paste"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_chords_parse_to_virtual_keys() {
        assert_eq!(parse_chord("ctrl+v"), Some(vec![0x11, 0x56]));
        assert_eq!(parse_chord("Shift+Insert"), Some(vec![0x10, 0x2D]));
        assert_eq!(parse_chord("ctrl+shift+v"), Some(vec![0x11, 0x10, 0x56]));
        assert_eq!(parse_chord("v"), Some(vec![0x56]));
        assert_eq!(parse_chord("hyper+v"), None);
        assert_eq!(parse_chord("ctrl+nonsense"), None);
    }

    #[test]
    fn invalid_paste_keys_fall_back_to_ctrl_v() {
        let out = WinPasteOutput::new(false, None, Some("bogus+".into()), 0, false, 0);
        assert_eq!(out.chord, vec![0x11, 0x56]);
        assert!(out.restore.is_none());
    }

    /// On the Windows CI runner: clipboard mode really sets the clipboard,
    /// including non-ASCII text and the appended suffix.
    #[cfg(windows)]
    #[tokio::test]
    async fn windows_clipboard_round_trip() {
        let out = WinClipboardOutput::new(Some(" ".into()));
        out.output("voxtype héllo 你好").await.unwrap();
        assert_eq!(get_clipboard().as_deref(), Some("voxtype héllo 你好 "));
    }
}
