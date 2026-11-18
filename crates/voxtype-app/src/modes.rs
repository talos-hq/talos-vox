//! Modes window: edit the `[[modes]]` list that picks a profile from the app
//! focused at record start (M3d; `docs/CONFIGURATION.md`, "Modes").
//!
//! Modes are an array of tables, which the scalar schema does not cover, so
//! this writes them with `toml_edit`: only the `modes` entries are replaced
//! and the rest of the file, comments included, is kept. The config loader
//! salvages per section, so a `[[modes]]` it could not read would be
//! dropped silently; after writing, the file is loaded again and must yield
//! exactly the modes written, or the original file is put back.

use std::path::{Path, PathBuf};

use eframe::egui;
use voxtype::config::Mode;

/// One mode as edited: lists are comma-separated text until saved.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Draft {
    pub name: String,
    pub apps: String,
    pub titles: String,
    pub profile: String,
}

impl Draft {
    fn from_mode(m: &Mode) -> Self {
        Self {
            name: m.name.clone().unwrap_or_default(),
            apps: m.app.join(", "),
            titles: m.title.join(", "),
            profile: m.profile.clone(),
        }
    }

    fn to_mode(&self) -> Mode {
        let list = |s: &str| {
            s.split(',')
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        Mode {
            name: Some(self.name.trim().to_string()).filter(|n| !n.is_empty()),
            app: list(&self.apps),
            title: list(&self.titles),
            profile: self.profile.trim().to_string(),
        }
    }
}

pub struct Modes {
    pub path: PathBuf,
    pub drafts: Vec<Draft>,
    saved: Vec<Draft>,
    /// `[profiles.NAME]` names, for the profile picker.
    pub profiles: Vec<String>,
    pub message: Option<Result<String, String>>,
}

impl Modes {
    pub fn new(path: PathBuf) -> Self {
        let mut m = Self {
            path,
            drafts: Vec::new(),
            saved: Vec::new(),
            profiles: Vec::new(),
            message: None,
        };
        m.reload();
        m
    }

    pub fn reload(&mut self) {
        let cfg = if self.path.exists() {
            match voxtype::config::load_config(Some(&self.path)) {
                Ok(cfg) => cfg,
                Err(e) => {
                    self.message = Some(Err(format!("{}: {e}", self.path.display())));
                    return;
                }
            }
        } else {
            voxtype::config::Config::default()
        };
        self.drafts = cfg.modes.iter().map(Draft::from_mode).collect();
        self.saved = self.drafts.clone();
        let mut names: Vec<String> = cfg.profiles.keys().cloned().collect();
        names.sort();
        self.profiles = names;
    }

    pub fn dirty(&self) -> bool {
        self.drafts != self.saved
    }

    /// Problems that would make a mode useless or unloadable.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (i, d) in self.drafts.iter().enumerate() {
            let m = d.to_mode();
            let label = m.name.clone().unwrap_or_else(|| format!("mode {}", i + 1));
            if m.profile.is_empty() {
                out.push(format!("{label}: pick a profile"));
            } else if !self.profiles.contains(&m.profile) {
                out.push(format!(
                    "{label}: no [profiles.{}] in config.toml",
                    m.profile
                ));
            }
            if m.app.is_empty() && m.title.is_empty() {
                out.push(format!("{label}: matches nothing (add an app or a title)"));
            }
        }
        out
    }

    pub fn save(&mut self) {
        let modes: Vec<Mode> = self.drafts.iter().map(Draft::to_mode).collect();
        self.message = Some(match write_modes(&self.path, &modes) {
            Ok(()) => {
                self.reload();
                Ok(format!(
                    "Saved {} mode(s). They apply from the next recording.",
                    modes.len()
                ))
            }
            Err(e) => Err(e),
        });
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.weak(self.path.display().to_string());
            if ui.small_button("Reload").clicked() {
                self.message = None;
                self.reload();
            }
        });
        ui.small(
            "The first mode whose app or title matches the window focused when \
             recording starts picks its profile. Run `voxtype context --watch` to \
             see app ids.",
        );
        if self.profiles.is_empty() {
            ui.small("No [profiles.NAME] tables in config.toml yet; add one to route to.");
        }
        ui.separator();

        let mut remove = None;
        let mut swap = None;
        let count = self.drafts.len();
        egui::ScrollArea::vertical()
            .max_height(ui.available_height() - 80.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (i, d) in self.drafts.iter_mut().enumerate() {
                    egui::Grid::new(("mode", i)).num_columns(2).show(ui, |ui| {
                        ui.label("Name");
                        ui.text_edit_singleline(&mut d.name);
                        ui.end_row();
                        ui.label("Apps");
                        ui.add(
                            egui::TextEdit::singleline(&mut d.apps)
                                .hint_text("code, org.wezfurlong.wezterm"),
                        );
                        ui.end_row();
                        ui.label("Title contains");
                        ui.add(egui::TextEdit::singleline(&mut d.titles).hint_text("- Slack"));
                        ui.end_row();
                        ui.label("Profile");
                        egui::ComboBox::from_id_salt(("profile", i))
                            .selected_text(d.profile.clone())
                            .show_ui(ui, |ui| {
                                for p in &self.profiles {
                                    ui.selectable_value(&mut d.profile, p.clone(), p);
                                }
                            });
                        ui.end_row();
                    });
                    ui.horizontal(|ui| {
                        if ui.add_enabled(i > 0, egui::Button::new("Up")).clicked() {
                            swap = Some((i - 1, i));
                        }
                        if ui
                            .add_enabled(i + 1 < count, egui::Button::new("Down"))
                            .clicked()
                        {
                            swap = Some((i, i + 1));
                        }
                        if ui.button("Remove").clicked() {
                            remove = Some(i);
                        }
                    });
                    ui.separator();
                }
            });
        if let Some((a, b)) = swap {
            self.drafts.swap(a, b);
        }
        if let Some(i) = remove {
            self.drafts.remove(i);
        }

        let problems = self.problems();
        for p in &problems {
            ui.colored_label(ui.visuals().warn_fg_color, p.as_str());
        }
        ui.horizontal(|ui| {
            if ui.button("Add mode").clicked() {
                self.drafts.push(Draft {
                    profile: self.profiles.first().cloned().unwrap_or_default(),
                    ..Draft::default()
                });
            }
            let can_save = self.dirty() && problems.is_empty();
            if ui
                .add_enabled(can_save, egui::Button::new("Save"))
                .clicked()
            {
                self.save();
            }
            if ui
                .add_enabled(self.dirty(), egui::Button::new("Discard"))
                .clicked()
            {
                self.drafts = self.saved.clone();
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
    }
}

/// Replace the `[[modes]]` array in `path`, keeping everything else.
pub fn write_modes(path: &Path, modes: &[Mode]) -> Result<(), String> {
    let original = match std::fs::read_to_string(path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let text = render(original.as_deref().unwrap_or(""), modes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    replace_checked(path, original.as_deref(), &text, |cfg| {
        if cfg.modes == modes {
            Ok(())
        } else {
            Err("the daemon would not read these modes back".to_string())
        }
    })
}

/// `config` with its `modes` replaced by `modes`.
fn render(config: &str, modes: &[Mode]) -> Result<String, toml_edit::TomlError> {
    let mut doc: toml_edit::DocumentMut = config.parse()?;
    doc.remove("modes");
    if !modes.is_empty() {
        let list = |v: &[String]| {
            let mut a = toml_edit::Array::new();
            v.iter().for_each(|s| a.push(s.as_str()));
            toml_edit::value(a)
        };
        let mut array = toml_edit::ArrayOfTables::new();
        for m in modes {
            let mut t = toml_edit::Table::new();
            if let Some(name) = &m.name {
                t["name"] = toml_edit::value(name.as_str());
            }
            if !m.app.is_empty() {
                t["app"] = list(&m.app);
            }
            if !m.title.is_empty() {
                t["title"] = list(&m.title);
            }
            t["profile"] = toml_edit::value(m.profile.as_str());
            array.push(t);
        }
        doc.insert("modes", toml_edit::Item::ArrayOfTables(array));
    }
    Ok(doc.to_string())
}

/// Write `text` to `path` (through a temp file and a rename), load it, and
/// run `check` on the result. On any failure put `original` back (or remove
/// the file if there was none).
fn replace_checked(
    path: &Path,
    original: Option<&str>,
    text: &str,
    check: impl Fn(&voxtype::config::Config) -> Result<(), String>,
) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("toml.voxtype-app.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    let verdict = voxtype::config::load_config(Some(path))
        .map_err(|e| e.to_string())
        .and_then(|cfg| check(&cfg));
    let Err(why) = verdict else {
        return Ok(());
    };
    let restore = match original {
        Some(text) => std::fs::write(path, text),
        None => std::fs::remove_file(path),
    };
    Err(match restore {
        Ok(()) => format!("not saved: {why}"),
        Err(e) => format!("{why}, and restoring {} failed: {e}", path.display()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "\
# my config
engine = \"whisper\"

[profiles.code]
replacements = { \"semi colon\" = \";\" }

[profiles.chat]

[[modes]]
app = [\"old\"]
profile = \"chat\"
";

    #[test]
    fn round_trips_and_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, BASE).unwrap();
        let mut m = Modes::new(path.clone());
        assert_eq!(m.drafts.len(), 1);
        assert_eq!(m.profiles, vec!["chat".to_string(), "code".to_string()]);

        m.drafts[0].apps = "Code, dev.zed.Zed ,".into();
        m.drafts[0].profile = "code".into();
        m.drafts.push(Draft {
            name: "slack".into(),
            titles: "- Slack".into(),
            profile: "chat".into(),
            ..Draft::default()
        });
        assert!(m.dirty() && m.problems().is_empty());
        m.save();
        assert!(matches!(m.message, Some(Ok(_))), "{:?}", m.message);
        assert!(!m.dirty());

        let cfg = voxtype::config::load_config(Some(&path)).unwrap();
        assert_eq!(cfg.modes.len(), 2);
        assert_eq!(cfg.modes[0].app, vec!["Code", "dev.zed.Zed"]);
        assert_eq!(cfg.modes[0].profile, "code");
        assert_eq!(cfg.modes[1].name.as_deref(), Some("slack"));
        assert_eq!(cfg.modes[1].title, vec!["- Slack"]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# my config") && text.contains("semi colon"),
            "{text}"
        );

        // Removing every mode removes the array.
        m.drafts.clear();
        m.save();
        let cfg = voxtype::config::load_config(Some(&path)).unwrap();
        assert!(cfg.modes.is_empty());
        assert!(!std::fs::read_to_string(&path)
            .unwrap()
            .contains("[[modes]]"));
    }

    #[test]
    fn problems_block_bad_modes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, BASE).unwrap();
        let mut m = Modes::new(path);
        m.drafts.push(Draft::default());
        let p = m.problems();
        assert!(p.iter().any(|x| x.contains("pick a profile")), "{p:?}");
        assert!(p.iter().any(|x| x.contains("matches nothing")), "{p:?}");
        m.drafts.last_mut().unwrap().profile = "missing".into();
        assert!(m
            .problems()
            .iter()
            .any(|x| x.contains("[profiles.missing]")));
    }

    #[test]
    fn a_failed_check_restores_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, BASE).unwrap();
        let err = replace_checked(&path, Some(BASE), "engine = \"whisper\"\n", |_| {
            Err("modes were dropped".into())
        })
        .unwrap_err();
        assert!(err.contains("not saved: modes were dropped"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), BASE);

        // With no original file, a failed write leaves no file.
        let fresh = dir.path().join("new/config.toml");
        assert!(replace_checked(&fresh, None, "", |_| Err("no".into())).is_err());
        assert!(!fresh.exists());
    }

    #[test]
    fn unparseable_config_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let broken = "this = = broken\n";
        std::fs::write(&path, broken).unwrap();
        assert!(write_modes(&path, &[]).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
    }

    #[test]
    fn renders_headless() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, BASE).unwrap();
        let mut m = Modes::new(path);
        m.drafts.push(Draft::default());
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| m.show(ui));
    }
}
