//! Settings window: every scalar key in `voxtype::config::schema`, the same
//! allowlist `voxtype config set` and the `voxtype configure` TUI use.
//!
//! Values shown are the effective ones (`schema::resolve`, so defaults
//! included). An edit goes through `config_set::set_key`, which validates
//! it against the schema and writes it with the TUI's comment-preserving
//! editor, then the config is re-read so the view shows what the daemon
//! will see. Nothing here talks to the daemon: most keys take effect at its
//! next start, and the row says so.

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui;
use serde_json::Value as Json;
use voxtype::config::schema::{self, KeySpec, KeyType};
use voxtype::config::Config;

/// The settings state, separate from drawing so it is testable.
pub struct Settings {
    pub path: PathBuf,
    /// Effective value of each scalar key.
    values: HashMap<&'static str, Json>,
    /// Text being typed into a string/number field, keyed by dotted key.
    drafts: HashMap<&'static str, String>,
    section: &'static str,
    /// Last write result, shown under the list.
    pub message: Option<Result<String, String>>,
}

impl Settings {
    pub fn new(path: PathBuf) -> Self {
        let mut s = Self {
            path,
            values: HashMap::new(),
            drafts: HashMap::new(),
            section: schema::SECTIONS[0],
            message: None,
        };
        s.reload();
        s
    }

    /// Re-read the file. A config that does not parse shows defaults and
    /// the parse error.
    pub fn reload(&mut self) {
        let cfg = if self.path.exists() {
            match voxtype::config::load_config(Some(&self.path)) {
                Ok(cfg) => cfg,
                Err(e) => {
                    self.message = Some(Err(format!("{}: {e}", self.path.display())));
                    Config::default()
                }
            }
        } else {
            Config::default()
        };
        self.values = schema::scalar_keys()
            .filter_map(|k| Some((k.key, schema::resolve(k.key, &cfg)?)))
            .collect();
        self.drafts.clear();
    }

    pub fn value(&self, key: &str) -> Option<&Json> {
        self.values.get(key)
    }

    /// Write one key. `raw` is what `voxtype config set KEY RAW` takes.
    pub fn set(&mut self, key: &'static str, raw: &str) {
        self.message = Some(
            match voxtype::config_set::set_key(self.path.clone(), key, raw) {
                Ok(out) => Ok(if out.restart_required {
                    format!("Saved {}. Restart the daemon to apply it.", out.key)
                } else {
                    format!("Saved {}.", out.key)
                }),
                Err(e) => Err(format!("{key}: {e}")),
            },
        );
        self.reload();
    }

    /// Remove one key from the file, back to its default.
    pub fn reset(&mut self, key: &'static str) {
        self.message = Some(
            match voxtype::config_set::unset_key(self.path.clone(), key) {
                Ok(out) => Ok(format!("Reset {} to its default.", out.key)),
                Err(e) => Err(format!("{key}: {e}")),
            },
        );
        self.reload();
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("settings-sections")
            .resizable(false)
            .show_inside(ui, |ui| {
                for section in schema::SECTIONS {
                    if schema::scalar_keys().any(|k| k.section == *section) {
                        ui.selectable_value(&mut self.section, section, *section);
                    }
                }
            });
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.weak(self.path.display().to_string());
                if ui.small_button("Reload").clicked() {
                    self.message = None;
                    self.reload();
                }
            });
            match &self.message {
                Some(Ok(m)) => {
                    ui.label(m.as_str());
                }
                Some(Err(e)) => {
                    ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                }
                None => {}
            }
            ui.separator();
            let section = self.section;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for spec in schema::scalar_keys().filter(|k| k.section == section) {
                        self.row(ui, spec);
                        ui.separator();
                    }
                });
        });
    }

    fn row(&mut self, ui: &mut egui::Ui, spec: &'static KeySpec) {
        let current = self.value(spec.key).cloned().unwrap_or(Json::Null);
        ui.horizontal(|ui| {
            ui.strong(spec.label);
            ui.weak(spec.key);
            if let Some(engine) = spec.engine {
                ui.weak(format!("(engine: {engine})"));
            }
            if !spec.restart_required {
                ui.weak("(live)");
            }
        });
        if !spec.description.is_empty() {
            ui.add(egui::Label::new(egui::RichText::new(spec.description).small()).wrap());
        }
        let compiled = spec.compiled();
        ui.add_enabled_ui(compiled, |ui| {
            ui.horizontal(|ui| {
                if let Some(raw) = self.widget(ui, spec, &current) {
                    self.set(spec.key, &raw);
                }
                if ui
                    .small_button("Default")
                    .on_hover_text("Remove the key from config.toml")
                    .clicked()
                {
                    self.reset(spec.key);
                }
            });
        });
        if !compiled {
            ui.small(format!(
                "Needs a build with the `{}` feature; set it with `voxtype config set`.",
                spec.requires_feature.unwrap_or_default()
            ));
        }
    }

    /// Draw the editor for one key. Returns the raw value to write when the
    /// user commits a change.
    fn widget(
        &mut self,
        ui: &mut egui::Ui,
        spec: &'static KeySpec,
        current: &Json,
    ) -> Option<String> {
        widget_kind(spec.ty)?;
        match spec.ty {
            KeyType::Bool => {
                let mut on = current.as_bool().unwrap_or(false);
                ui.checkbox(&mut on, "").changed().then(|| on.to_string())
            }
            KeyType::Enum { choices, open } => {
                let shown = json_text(current);
                let mut picked = None;
                egui::ComboBox::from_id_salt(spec.key)
                    .selected_text(shown.clone())
                    .show_ui(ui, |ui| {
                        for c in choices {
                            if ui.selectable_label(shown == *c, *c).clicked() && shown != *c {
                                picked = Some(c.to_string());
                            }
                        }
                    });
                if open {
                    picked = picked.or_else(|| self.text(ui, spec.key, &shown));
                }
                picked
            }
            KeyType::Int { .. }
            | KeyType::Float { .. }
            | KeyType::String
            | KeyType::DynamicEnum { .. } => {
                let shown = json_text(current);
                self.text(ui, spec.key, &shown)
            }
            KeyType::MapString => None,
        }
    }

    /// A single-line field committed on Enter or focus loss, only when the
    /// text differs from the stored value.
    fn text(&mut self, ui: &mut egui::Ui, key: &'static str, shown: &str) -> Option<String> {
        let draft = self.drafts.entry(key).or_insert_with(|| shown.to_string());
        let resp = ui.add(egui::TextEdit::singleline(draft).desired_width(220.0));
        let committed = resp.lost_focus() && draft.trim() != shown;
        committed.then(|| draft.trim().to_string())
    }
}

/// A JSON value as `voxtype config set` would take it.
pub fn json_text(v: &Json) -> String {
    match v {
        Json::Null => String::new(),
        Json::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The widget kind a key gets. Every scalar key must have one; the test
/// below is the ADR's "every schema key rendered by the app" check.
pub fn widget_kind(ty: KeyType) -> Option<&'static str> {
    match ty {
        KeyType::Bool => Some("checkbox"),
        KeyType::Enum { open: false, .. } => Some("combo"),
        KeyType::Enum { open: true, .. } => Some("combo+text"),
        KeyType::Int { .. } | KeyType::Float { .. } => Some("number"),
        KeyType::String | KeyType::DynamicEnum { .. } => Some("text"),
        KeyType::MapString => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_schema_key_has_a_widget_and_a_value() {
        let s = Settings::new(PathBuf::from("/nonexistent/voxtype-app/config.toml"));
        for spec in schema::scalar_keys() {
            assert!(widget_kind(spec.ty).is_some(), "{} has no widget", spec.key);
            assert!(
                s.value(spec.key).is_some(),
                "{} has no resolved value",
                spec.key
            );
            assert!(
                schema::SECTIONS.contains(&spec.section),
                "{} is in unknown section {}",
                spec.key,
                spec.section
            );
        }
    }

    #[test]
    fn set_writes_the_file_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# my settings\n[history]\nenabled = false\n").unwrap();
        let mut s = Settings::new(path.clone());
        assert_eq!(s.value("history.enabled"), Some(&Json::Bool(false)));

        s.set("history.enabled", "true");
        assert!(matches!(s.message, Some(Ok(_))), "{:?}", s.message);
        assert_eq!(s.value("history.enabled"), Some(&Json::Bool(true)));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my settings"), "comments survive: {text}");

        // Invalid values are refused and the file is unchanged.
        s.set("history.retention_days", "not-a-number");
        assert!(matches!(s.message, Some(Err(_))));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);

        s.reset("history.enabled");
        assert_eq!(s.value("history.enabled"), Some(&Json::Bool(false)));
    }

    #[test]
    fn renders_every_section_headless() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Settings::new(dir.path().join("config.toml"));
        let ctx = egui::Context::default();
        for section in schema::SECTIONS {
            s.section = section;
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| s.show(ui));
        }
    }
}
