//! Text injection through kernal-api's desktop input: `SendInput` on
//! Windows, `CGEventPost` on macOS. Types Unicode at the focused control,
//! independent of keyboard layout.

use super::TextOutput;
use crate::error::OutputError;
use kernal_api::desktop_input::{self, InputError, MAX_TEXT_CHARS};

pub struct DesktopInputOutput {
    auto_submit: bool,
    append_text: Option<String>,
}

impl DesktopInputOutput {
    pub fn new(auto_submit: bool, append_text: Option<String>) -> Self {
        Self {
            auto_submit,
            append_text,
        }
    }

    /// The full text to type, split into chunks the facade accepts.
    fn chunks(&self, text: &str) -> Vec<String> {
        let mut full = text.to_string();
        if let Some(append) = &self.append_text {
            full.push_str(append);
        }
        if self.auto_submit {
            full.push('\n');
        }
        let chars: Vec<char> = full.chars().filter(|c| *c != '\0').collect();
        chars
            .chunks(MAX_TEXT_CHARS)
            .map(|chunk| chunk.iter().collect())
            .collect()
    }
}

fn describe(error: InputError) -> String {
    match error {
        InputError::InvalidInput => "text rejected by the input facility".into(),
        InputError::Unsupported => "this host has no input injection".into(),
        InputError::Unavailable => "input injection is unavailable".into(),
        InputError::Denied => "input injection was denied (check accessibility permission)".into(),
    }
}

#[async_trait::async_trait]
impl TextOutput for DesktopInputOutput {
    async fn output(&self, text: &str) -> Result<(), OutputError> {
        for chunk in self.chunks(text) {
            desktop_input::type_text(&chunk)
                .await
                .map_err(|e| OutputError::InjectionFailed(describe(e)))?;
        }
        Ok(())
    }

    async fn is_available(&self) -> bool {
        true
    }

    fn name(&self) -> &'static str {
        "desktop-input"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_append_text_and_submit_within_the_facade_limit() {
        let output = DesktopInputOutput::new(true, Some(" ".to_string()));
        let long = "a".repeat(MAX_TEXT_CHARS + 1);
        let chunks = output.chunks(&long);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), MAX_TEXT_CHARS);
        assert_eq!(chunks[1], "a \n");
    }
}
