//! Top-level subcommand enum.

use clap::builder::PossibleValuesParser;
use clap::Subcommand;

use super::{ConfigAction, HistoryAction, InfoAction, MeetingAction, RecordAction, SetupAction};

#[derive(Subcommand)]
pub enum Commands {
    /// Run as daemon (default if no command specified)
    Daemon,

    /// Run menu bar helper (macOS)
    #[cfg(target_os = "macos")]
    Menubar,

    /// Launch daemon + menubar (used by Voxtype.app bundle)
    #[cfg(target_os = "macos")]
    #[command(hide = true)]
    AppLaunch,

    /// Transcribe an audio file (WAV, 16kHz, mono)
    Transcribe {
        /// Path to audio file
        file: std::path::PathBuf,

        /// Override transcription engine
        #[arg(
            long,
            value_name = "ENGINE",
            long_help = format!("Override transcription engine: {}", super::ENGINE_NAMES_CSV),
        )]
        engine: Option<String>,
    },

    /// Internal: Worker process for GPU-isolated transcription
    /// Reads audio from stdin, writes transcription result to stdout
    #[command(hide = true)]
    TranscribeWorker {
        /// Model name or path (passed from parent process)
        #[arg(long)]
        model: Option<String>,

        /// Language code (passed from parent process)
        #[arg(long)]
        language: Option<String>,

        /// Enable translation to English (passed from parent process)
        #[arg(long)]
        translate: bool,

        /// Number of threads for inference (passed from parent process)
        #[arg(long)]
        threads: Option<usize>,
    },

    /// Setup and installation utilities
    Setup {
        #[command(subcommand)]
        action: Option<SetupAction>,

        /// Download model if missing (shorthand for basic setup)
        #[arg(long)]
        download: bool,

        /// Specify which model to download (use with --download).
        /// Whisper: tiny, base, small, medium, large-v3, large-v3-turbo (and .en variants).
        /// Parakeet: parakeet-tdt-0.6b-v3, parakeet-tdt-0.6b-v3-int8.
        /// Other engines (Moonshine, SenseVoice, Paraformer, Dolphin,
        /// Omnilingual, Cohere, OpenVINO) take the directory-form names shown
        /// by `voxtype info models`, e.g. cohere-transcribe-q4f16.
        #[arg(long, value_name = "NAME")]
        model: Option<String>,

        /// Suppress all output (for scripting/automation)
        #[arg(long)]
        quiet: bool,

        /// Suppress only "Next steps" instructions
        #[arg(long)]
        no_post_install: bool,

        /// Also switch the config to use the model, the way the interactive
        /// picker does: sets `engine` and `<engine>.model`.
        ///
        /// Off by default. Downloading a model does not select it, so a GUI's
        /// Download button and scripted pre-downloads can't change which
        /// engine the daemon loads. Select a model explicitly with
        /// `voxtype config set <engine>.model <NAME>`.
        #[arg(long)]
        activate: bool,

        /// How download progress is reported: human (curl's progress bar) or
        /// json (one NDJSON event per update on stdout, for a GUI to render).
        ///
        /// json implies --quiet for human status lines; stdout carries only
        /// events. Failures still exit non-zero, with an "error" event
        /// alongside.
        #[arg(
            long,
            value_name = "FORMAT",
            default_value = "human",
            value_parser = PossibleValuesParser::new(super::PROGRESS_FORMATS),
            env = "VOXTYPE_PROGRESS_FORMAT",
        )]
        progress_format: String,
    },

    /// Show or modify configuration
    ///
    /// With no subcommand, prints the resolved configuration. Use `voxtype
    /// config set engine <NAME>` to change the active transcription engine
    /// in the on-disk config file (preserving comments and other settings).
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },

    /// Inspect runtime/install information
    Info {
        #[command(subcommand)]
        action: InfoAction,
    },

    /// Open the interactive configuration TUI
    Configure {
        /// Render as if installed from a package (for testing source builds).
        #[arg(long, hide = true)]
        force_package_mode: bool,

        /// Print detected audio input devices (one per line) and exit.
        /// Used internally by the TUI to probe devices in a subprocess, so
        /// an ALSA hang or crash during probing can't take the TUI with it.
        #[arg(long, hide = true)]
        probe_audio_devices: bool,
    },

    /// Show daemon status (for Waybar/polybar integration)
    Status {
        /// Continuously output status changes as JSON (for Waybar exec)
        #[arg(long)]
        follow: bool,

        /// Output format: "text" (default) or "json" (for Waybar)
        #[arg(long, default_value = "text")]
        format: String,

        /// Include extended info in JSON (model, device, backend)
        #[arg(long)]
        extended: bool,

        /// Icon theme for JSON output (emoji, nerd-font, material, phosphor, codicons, omarchy, minimal, dots, arrows, text, or path to custom theme)
        #[arg(long, value_name = "THEME")]
        icon_theme: Option<String>,
    },

    /// Control recording from external sources (compositor keybindings, scripts)
    Record {
        #[command(subcommand)]
        action: RecordAction,
    },

    /// Meeting transcription mode
    ///
    /// Continuous meeting transcription with chunked processing,
    /// speaker attribution, and export capabilities.
    Meeting {
        #[command(subcommand)]
        action: MeetingAction,
    },

    /// Check for updates
    CheckUpdate,

    /// Show or clear dictation history
    ///
    /// History is off by default. Enable it in config.toml:
    ///
    ///   [history]
    ///   enabled = true        # record each dictation's final text
    ///   retention_days = 7    # 0 = no age limit
    ///   max_entries = 500
    ///
    /// Only text is stored, never audio, in the per-OS state directory
    /// (Linux: ~/.local/state/voxtype/history.db).
    History {
        #[command(subcommand)]
        action: HistoryAction,
    },

    /// Show which application has focus, as voxtype sees it
    ///
    /// Prints one JSON object: the probe this session uses (hyprland, sway,
    /// x11, macos or windows; null where none applies, such as GNOME/KDE
    /// on Wayland) and the focused app_id and title. This is what
    /// VOXTYPE_APP_ID / VOXTYPE_APP_TITLE and [[modes]] see at record start.
    Context {
        /// Keep running and print a line whenever the focused app changes
        #[arg(long)]
        watch: bool,
    },

    /// Type the most recent dictation again
    ///
    /// Sends the newest history entry through the configured output chain,
    /// as if it had just been dictated. Needs [history] enabled = true.
    /// Bind it to a key, e.g. Hyprland:
    ///   bind = SUPER CTRL, V, exec, voxtype paste-last
    PasteLast,

    /// Serve the Model Context Protocol on stdin/stdout
    ///
    /// For agent hosts (Claude, Hermes, Talos) that launch MCP servers as a
    /// subprocess. Tools: `status` (the `status --format json --extended`
    /// object) and `transcribe_file` (same as `voxtype transcribe`). Stdio
    /// only: nothing listens on a socket or port. Logs go to stderr.
    ///
    /// Example client config: {"command": "voxtype", "args": ["mcp"]}
    #[cfg(feature = "mcp")]
    Mcp,
}

impl Commands {
    /// Commands whose stdout is a machine protocol, so logs must go to stderr.
    pub fn logs_to_stderr(&self) -> bool {
        match self {
            Commands::TranscribeWorker { .. } => true,
            #[cfg(feature = "mcp")]
            Commands::Mcp => true,
            _ => false,
        }
    }
}
