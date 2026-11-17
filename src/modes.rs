//! Modes: route a dictation to a profile by the app focused at record start.
//!
//! Resolution, in order: an explicit `--profile` always wins; otherwise the
//! first `[[modes]]` entry matching the snapshot taken at record start
//! (`crate::context`) names the profile; otherwise no profile. Where no
//! focus probe exists (GNOME/KDE on Wayland) there is no snapshot, so no
//! mode matches and behaviour is exactly the global configuration.

use crate::config::{Config, Mode, Profile, TextConfig};
use crate::context::AppContext;

fn matches(mode: &Mode, app: &AppContext) -> bool {
    let app_hit = mode.app.iter().any(|a| a.eq_ignore_ascii_case(&app.app_id));
    let title_hit = app.title.as_deref().is_some_and(|title| {
        let title = title.to_lowercase();
        mode.title
            .iter()
            .any(|t| !t.is_empty() && title.contains(&t.to_lowercase()))
    });
    app_hit || title_hit
}

/// The profile name the first matching mode selects for `app`.
pub fn resolve<'a>(modes: &'a [Mode], app: Option<&AppContext>) -> Option<&'a Mode> {
    let app = app?;
    modes.iter().find(|m| matches(m, app))
}

/// Daemon hook: the profile for this recording when no `--profile` was
/// given, from the snapshot taken at record start.
pub fn profile_for(config: &Config) -> Option<String> {
    if config.modes.is_empty() {
        return None;
    }
    let app = crate::context::captured();
    let mode = resolve(&config.modes, app.as_ref())?;
    tracing::info!(
        "Mode {:?} matched app {:?}: profile {:?}",
        mode.name.as_deref().unwrap_or("(unnamed)"),
        app.as_ref().map(|a| a.app_id.as_str()),
        mode.profile
    );
    Some(mode.profile.clone())
}

/// Daemon hook: apply the active profile's `replacements` on top of the
/// global text processing. Text is returned unchanged without a profile or
/// when it has no replacements.
pub fn apply_profile_replacements(profile: Option<&Profile>, text: String) -> String {
    let Some(profile) = profile.filter(|p| !p.replacements.is_empty()) else {
        return text;
    };
    let config = TextConfig {
        replacements: profile.replacements.clone(),
        spoken_punctuation: false,
        smart_auto_submit: false,
        filter_filler_words: false,
        filler_words: Vec::new(),
    };
    crate::text::TextProcessor::new(&config).process(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(id: &str, title: Option<&str>) -> AppContext {
        AppContext {
            app_id: id.into(),
            title: title.map(str::to_string),
            source: "test",
        }
    }

    fn config() -> Config {
        toml::from_str(
            r#"
            [profiles.code]
            replacements = { "open paren" = "(" , "vox type" = "voxtype" }

            [profiles.chat]
            replacements = { "vox type" = "Voxtype!" }

            [[modes]]
            name = "editors"
            app = ["code", "com.microsoft.VSCode"]
            profile = "code"

            [[modes]]
            title = ["- slack"]
            profile = "chat"

            [[modes]]
            app = ["code"]
            profile = "never-reached"
            "#,
        )
        .unwrap()
    }

    #[test]
    fn first_match_wins_and_matching_ignores_case() {
        let cfg = config();
        let hit = |a: AppContext| resolve(&cfg.modes, Some(&a)).map(|m| m.profile.as_str());
        assert_eq!(hit(app("Code", None)), Some("code"));
        assert_eq!(hit(app("com.microsoft.vscode", None)), Some("code"));
        assert_eq!(hit(app("firefox", Some("General - Slack"))), Some("chat"));
        assert_eq!(hit(app("firefox", Some("Inbox"))), None);
        assert_eq!(hit(app("firefox", None)), None);
    }

    #[test]
    fn no_snapshot_means_no_mode() {
        // GNOME/KDE on Wayland: no probe, no snapshot, global behaviour.
        assert!(resolve(&config().modes, None).is_none());
    }

    #[test]
    fn empty_title_patterns_never_match() {
        let modes = vec![Mode {
            title: vec![String::new()],
            profile: "x".into(),
            ..Mode::default()
        }];
        assert!(resolve(&modes, Some(&app("a", Some("anything")))).is_none());
    }

    /// The acceptance test from #3: the same dictation into two apps gets
    /// different replacements, and an unmatched app is untouched.
    #[test]
    fn same_dictation_two_apps_different_replacements() {
        let cfg = config();
        let text = "I use vox type daily";
        let run = |a: Option<AppContext>| {
            let profile = resolve(&cfg.modes, a.as_ref()).and_then(|m| cfg.get_profile(&m.profile));
            apply_profile_replacements(profile, text.to_string())
        };
        assert_eq!(run(Some(app("code", None))), "I use voxtype daily");
        assert_eq!(
            run(Some(app("slack", Some("random - Slack")))),
            "I use Voxtype! daily"
        );
        assert_eq!(run(Some(app("firefox", None))), text);
        assert_eq!(run(None), text);
    }

    #[test]
    fn configs_without_modes_parse_and_resolve_nothing() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.modes.is_empty());
        assert_eq!(profile_for(&cfg), None);
    }
}
