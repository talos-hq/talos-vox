//! Models tab: each engine's catalog with what is downloaded, downloads
//! with progress, verification, and picking the active model.
//!
//! Nothing here reimplements the model code:
//! - The catalog and the on-disk checks are `voxtype::model_catalog`, the
//!   same functions `voxtype info models` and the TUI's picker use.
//! - A download runs `voxtype setup --download --model NAME
//!   --progress-format json --no-post-install`, the CLI's own downloader.
//!   It writes to a `.part` file and renames only a checked file. R2 models
//!   (every engine but Whisper) are sha256-checked against their manifest
//!   file by file. Whisper files are checked against the server's size and
//!   the ggml header, because upstream publishes no checksums.
//! - After a download the model is verified again here
//!   (`model_catalog::verify_model_in`), and "Verify" re-hashes on demand.
//! - "Use" writes `engine` and `<engine>.model` through
//!   `config_set::set_key`, as `voxtype config set` does. Downloading never
//!   changes the config (no `--activate`).

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use eframe::egui;
use voxtype::model_catalog::{self, ModelHealth, ModelVerification};

/// One catalog entry and what is on disk for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub name: &'static str,
    pub health: ModelHealth,
}

/// What a running job reports.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Progress {
        file: String,
        bytes: u64,
        total: Option<u64>,
    },
    /// The job ended: a summary, or why it failed.
    Done(Result<String, String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Download,
    Verify,
}

/// A download or verification in flight. One at a time.
pub struct Job {
    pub engine: &'static str,
    pub model: &'static str,
    pub kind: JobKind,
    pub progress: Option<(String, u64, Option<u64>)>,
    events: Receiver<Event>,
    child: Option<Arc<Mutex<Child>>>,
}

impl Job {
    /// Stop a download, with the `curl` the CLI runs under it. The
    /// downloader leaves only a `.part` file, which the next download
    /// replaces.
    pub fn cancel(&self) {
        if let Some(child) = &self.child {
            kill_tree(&mut child.lock().unwrap_or_else(|e| e.into_inner()));
        }
    }
}

pub struct Models {
    /// The config file "Use" writes.
    pub config: PathBuf,
    /// Where models live (`Config::models_dir()`).
    pub models_dir: PathBuf,
    /// The voxtype binary that downloads.
    pub voxtype: PathBuf,
    /// The engine shown.
    pub engine: &'static str,
    /// The configured engine and model.
    pub active: (String, String),
    pub rows: Vec<Row>,
    pub job: Option<Job>,
    pub message: Option<Result<String, String>>,
    /// Set after "Use" when the daemon has to restart to load the model;
    /// the app restarts a daemon it supervises.
    pub restart_wanted: bool,
}

impl Models {
    pub fn new(config: PathBuf, models_dir: PathBuf, voxtype: PathBuf) -> Self {
        let mut m = Self {
            config,
            models_dir,
            voxtype,
            engine: model_catalog::CATALOG_ENGINES[0],
            active: (String::new(), String::new()),
            rows: Vec::new(),
            job: None,
            message: None,
            restart_wanted: false,
        };
        m.reload_active();
        if let Some(e) = model_catalog::CATALOG_ENGINES
            .iter()
            .find(|e| **e == m.active.0)
        {
            m.engine = e;
        }
        m.refresh();
        m
    }

    /// Re-read the configured engine and model.
    pub fn reload_active(&mut self) {
        let cfg = if self.config.exists() {
            voxtype::config::load_config(Some(&self.config)).unwrap_or_default()
        } else {
            voxtype::config::Config::default()
        };
        let engine = voxtype::config::schema::resolve("engine", &cfg)
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let model = voxtype::config::schema::resolve(&format!("{engine}.model"), &cfg)
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        self.active = (engine, model);
    }

    /// Re-check the shown engine's models on disk (cheap: stat and a
    /// header read, no hashing).
    pub fn refresh(&mut self) {
        self.rows = model_catalog::model_catalog(self.engine)
            .into_iter()
            .map(|name| Row {
                name,
                health: model_catalog::model_health_in(&self.models_dir, self.engine, name),
            })
            .collect();
    }

    pub fn set_engine(&mut self, engine: &'static str) {
        if self.engine != engine {
            self.engine = engine;
            self.refresh();
        }
    }

    /// Whether this build has the shown engine. Downloads go through the
    /// voxtype binary shipped with the app, built with the same features;
    /// `voxtype config set engine` refuses an engine that isn't compiled in.
    pub fn engine_compiled(&self) -> bool {
        voxtype::config_set::engine_feature_compiled(self.engine)
    }

    pub fn is_active(&self, model: &str) -> bool {
        self.active.0 == self.engine && self.active.1 == model
    }

    /// Start downloading `model` of the shown engine.
    pub fn download(&mut self, model: &'static str) {
        if self.job.is_some() {
            return;
        }
        if !self.engine_compiled() {
            self.message = Some(Err(not_compiled(self.engine)));
            return;
        }
        let Some(arg) = model_catalog::download_arg(self.engine, model) else {
            self.message = Some(Err(format!("{model}: no download for this engine")));
            return;
        };
        let mut cmd = Command::new(&self.voxtype);
        cmd.args([
            "setup",
            "--download",
            "--model",
            &arg,
            "--progress-format",
            "json",
            "--no-post-install",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        crate::supervisor::prepare_child(&mut cmd);
        // Its own process group, so Cancel reaches curl too.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.message = Some(Err(format!(
                    "could not run {}: {e}",
                    self.voxtype.display()
                )));
                return;
            }
        };
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let child = Arc::new(Mutex::new(child));
        let (tx, rx) = mpsc::channel();
        let (engine, models_dir, waited) = (self.engine, self.models_dir.clone(), child.clone());
        std::thread::spawn(move || {
            let done = run_download(stdout, stderr, &waited, &tx)
                .and_then(|()| verify_summary(&models_dir, engine, model, true));
            let _ = tx.send(Event::Done(done));
        });
        self.message = None;
        self.job = Some(Job {
            engine: self.engine,
            model,
            kind: JobKind::Download,
            progress: None,
            events: rx,
            child: Some(child),
        });
    }

    /// Hash an installed model against its manifest, off the UI thread.
    pub fn verify(&mut self, model: &'static str) {
        if self.job.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let (engine, models_dir) = (self.engine, self.models_dir.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Event::Done(verify_summary(
                &models_dir,
                engine,
                model,
                false,
            )));
        });
        self.message = None;
        self.job = Some(Job {
            engine,
            model,
            kind: JobKind::Verify,
            progress: None,
            events: rx,
            child: None,
        });
    }

    /// Make `model` the active one: `engine` and `<engine>.model` in the
    /// config, validated like `voxtype config set`.
    pub fn select(&mut self, model: &'static str) {
        let engine = self.engine;
        let set = |key: &str, value: &str| {
            voxtype::config_set::set_key(self.config.clone(), key, value)
                .map(|outcome| outcome.restart_required)
                .map_err(|e| format!("{key} = {value}: {e}"))
        };
        let result =
            set("engine", engine).and_then(|a| Ok(set(&format!("{engine}.model"), model)? || a));
        self.message = Some(match result {
            Ok(restart) => {
                self.restart_wanted = restart;
                Ok(format!("Active model: {engine} / {model}."))
            }
            Err(e) => Err(e),
        });
        self.reload_active();
    }

    /// Take the job's events. Returns true while a job runs.
    pub fn poll(&mut self) -> bool {
        let Some(job) = &mut self.job else {
            return false;
        };
        while let Ok(event) = job.events.try_recv() {
            match event {
                Event::Progress { file, bytes, total } => job.progress = Some((file, bytes, total)),
                Event::Done(result) => {
                    let what = format!("{} / {}", job.engine, job.model);
                    self.message = Some(match result {
                        Ok(s) => Ok(format!("{what}: {s}")),
                        Err(e) => Err(format!("{what}: {e}")),
                    });
                    self.job = None;
                    self.refresh();
                    return false;
                }
            }
        }
        true
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        if self.poll() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        ui.horizontal(|ui| {
            ui.label("Engine");
            let mut engine = self.engine;
            egui::ComboBox::from_id_salt("models-engine")
                .selected_text(engine)
                .show_ui(ui, |ui| {
                    for e in model_catalog::CATALOG_ENGINES {
                        ui.selectable_value(&mut engine, e, *e);
                    }
                });
            self.set_engine(engine);
            if ui.small_button("Rescan").clicked() {
                self.refresh();
            }
        });
        ui.small(format!(
            "Active: {} / {}   Models in {}",
            self.active.0,
            self.active.1,
            self.models_dir.display()
        ));
        let compiled = self.engine_compiled();
        if !compiled {
            ui.colored_label(ui.visuals().warn_fg_color, not_compiled(self.engine));
        }
        ui.separator();

        if let Some(job) = &self.job {
            ui.horizontal(|ui| {
                match (job.kind, &job.progress) {
                    (JobKind::Verify, _) => {
                        ui.spinner();
                        ui.label(format!("Verifying {}...", job.model));
                    }
                    (JobKind::Download, None) => {
                        ui.spinner();
                        ui.label(format!("Starting the download of {}...", job.model));
                    }
                    (JobKind::Download, Some((file, bytes, total))) => {
                        let bar = match total {
                            Some(t) if *t > 0 => egui::ProgressBar::new(*bytes as f32 / *t as f32)
                                .text(format!("{file}: {} / {}", mb(*bytes), mb(*t))),
                            _ => egui::ProgressBar::new(0.0)
                                .animate(true)
                                .text(format!("{file}: {}", mb(*bytes))),
                        };
                        ui.add(bar.desired_width(260.0));
                    }
                }
                if job.kind == JobKind::Download && ui.button("Cancel").clicked() {
                    job.cancel();
                }
            });
            ui.separator();
        }

        let busy = self.job.is_some() || !compiled;
        let mut action: Option<(&'static str, JobKind, bool)> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, true])
            .max_height(ui.available_height() - 40.0)
            .show(ui, |ui| {
                egui::Grid::new("models-grid")
                    .num_columns(3)
                    .striped(true)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        for row in &self.rows {
                            let active = self.is_active(row.name);
                            if active {
                                ui.strong(format!("\u{25CF} {}", row.name));
                            } else {
                                ui.label(row.name);
                            }
                            match &row.health {
                                ModelHealth::Present => {
                                    ui.label(if active {
                                        "Downloaded, active"
                                    } else {
                                        "Downloaded"
                                    });
                                }
                                ModelHealth::Missing => {
                                    ui.weak("Not downloaded");
                                }
                                ModelHealth::Corrupt(problems) => {
                                    ui.colored_label(ui.visuals().error_fg_color, "Damaged")
                                        .on_hover_text(problems.join("\n"));
                                }
                            }
                            ui.horizontal(|ui| match &row.health {
                                ModelHealth::Present => {
                                    if ui
                                        .add_enabled(!busy && !active, egui::Button::new("Use"))
                                        .clicked()
                                    {
                                        action = Some((row.name, JobKind::Download, true));
                                    }
                                    if ui.add_enabled(!busy, egui::Button::new("Verify")).clicked()
                                    {
                                        action = Some((row.name, JobKind::Verify, false));
                                    }
                                }
                                _ => {
                                    if ui
                                        .add_enabled(!busy, egui::Button::new("Download"))
                                        .clicked()
                                    {
                                        action = Some((row.name, JobKind::Download, false));
                                    }
                                }
                            });
                            ui.end_row();
                        }
                    });
            });
        match action {
            Some((name, _, true)) => self.select(name),
            Some((name, JobKind::Verify, _)) => self.verify(name),
            Some((name, JobKind::Download, _)) => self.download(name),
            None => {}
        }
        if let Some(msg) = &self.message {
            ui.separator();
            match msg {
                Ok(m) => ui.label(m.as_str()),
                Err(e) => ui.colored_label(ui.visuals().error_fg_color, e.as_str()),
            };
        }
    }
}

/// Kill the download and everything it started.
fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    if let Ok(pgid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: kill(2) on the process group the child leads (spawned
        // with process_group(0)).
        unsafe { libc::kill(-pgid, libc::SIGKILL) };
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .creation_flags(0x0800_0000)
            .status();
    }
    let _ = child.kill();
}

fn not_compiled(engine: &str) -> String {
    format!("This build has no {engine} engine, so its models can't be downloaded or used here.")
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

/// Read the CLI's NDJSON progress until it exits. `Ok` only for exit 0.
fn run_download(
    stdout: impl Read,
    mut stderr: impl Read + Send + 'static,
    child: &Mutex<Child>,
    tx: &Sender<Event>,
) -> Result<(), String> {
    // Drain stderr alongside, so a chatty child never blocks on a full pipe.
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let mut reported: Option<String> = None;
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        match parse_event(&line) {
            Some(Ok(p)) => {
                let _ = tx.send(p);
            }
            Some(Err(message)) => reported = Some(message),
            None => {}
        }
    }
    let status = child.lock().unwrap_or_else(|e| e.into_inner()).wait();
    let stderr = err_reader.join().unwrap_or_default();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(reported
            .or_else(|| last_line(&stderr))
            .unwrap_or_else(|| format!("download failed ({s})"))),
        Err(e) => Err(format!("download failed: {e}")),
    }
}

fn last_line(s: &str) -> Option<String> {
    s.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// One line of `--progress-format json`: `Some(Ok(progress))`,
/// `Some(Err(message))` for an error event, `None` for anything else.
pub fn parse_event(line: &str) -> Option<Result<Event, String>> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    match v.get("event")?.as_str()? {
        "progress" => Some(Ok(Event::Progress {
            file: v.get("file")?.as_str()?.to_string(),
            bytes: v.get("bytes")?.as_u64()?,
            total: v.get("total").and_then(serde_json::Value::as_u64),
        })),
        "error" => Some(Err(v
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("download failed")
            .to_string())),
        _ => None,
    }
}

/// Verify a model on disk and say what was established.
fn verify_summary(
    models_dir: &Path,
    engine: &str,
    model: &str,
    downloaded: bool,
) -> Result<String, String> {
    let lead = if downloaded { "downloaded; " } else { "" };
    match model_catalog::verify_model_in(models_dir, engine, model) {
        ModelVerification::Ok => Ok(format!("{lead}sha256 of every file matches the manifest")),
        ModelVerification::Corrupt(problems) => Err(problems.join("; ")),
        ModelVerification::Unverifiable(why) => {
            if model_catalog::model_health_in(models_dir, engine, model) == ModelHealth::Missing {
                Err("not on disk".into())
            } else {
                Ok(format!("{lead}{why}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn progress_events_parse() {
        assert_eq!(
            parse_event(
                r#"{"event":"progress","model":"base.en","file":"ggml-base.en.bin","bytes":5,"total":10,"pct":50.0}"#
            ),
            Some(Ok(Event::Progress {
                file: "ggml-base.en.bin".into(),
                bytes: 5,
                total: Some(10)
            }))
        );
        assert_eq!(
            parse_event(
                r#"{"event":"progress","model":"m","file":"f","bytes":7,"total":null,"pct":null}"#
            ),
            Some(Ok(Event::Progress {
                file: "f".into(),
                bytes: 7,
                total: None
            }))
        );
        assert_eq!(
            parse_event(r#"{"event":"error","model":"m","message":"404"}"#),
            Some(Err("404".into()))
        );
        assert_eq!(parse_event(r#"{"event":"done","model":"m"}"#), None);
        assert_eq!(parse_event("Downloading..."), None);
    }

    fn picker(dir: &Path, voxtype: PathBuf) -> Models {
        let models = dir.join("models");
        std::fs::create_dir_all(&models).unwrap();
        Models::new(dir.join("config.toml"), models, voxtype)
    }

    /// Poll until the job ends (at most 20 s).
    fn finish(m: &mut Models) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while m.poll() && Instant::now() < deadline {
            std::thread::park_timeout(Duration::from_millis(20));
        }
        assert!(m.job.is_none(), "job still running");
    }

    fn row<'a>(m: &'a Models, name: &str) -> &'a Row {
        m.rows.iter().find(|r| r.name == name).unwrap()
    }

    #[test]
    fn lists_every_engines_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = picker(dir.path(), PathBuf::from("voxtype"));
        // No config: whisper / base.en, the defaults.
        assert_eq!(m.active, ("whisper".into(), "base.en".into()));
        assert_eq!(m.engine, "whisper");
        assert!(m.rows.iter().any(|r| r.name == "base.en"));
        assert!(m.rows.iter().all(|r| r.health == ModelHealth::Missing));
        for engine in model_catalog::CATALOG_ENGINES {
            m.set_engine(engine);
            assert_eq!(m.rows.len(), model_catalog::model_catalog(engine).len());
        }
    }

    #[test]
    fn a_damaged_model_is_reported_and_fails_verification() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = picker(dir.path(), PathBuf::from("voxtype"));
        std::fs::write(m.models_dir.join("ggml-tiny.en.bin"), b"<html>404</html>").unwrap();
        m.refresh();
        assert!(matches!(row(&m, "tiny.en").health, ModelHealth::Corrupt(_)));
        m.verify("tiny.en");
        finish(&mut m);
        assert!(matches!(&m.message, Some(Err(e)) if e.contains("not a ggml model")));
    }

    #[test]
    fn use_writes_engine_and_model_to_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = picker(dir.path(), PathBuf::from("voxtype"));
        std::fs::write(m.models_dir.join("ggml-tiny.en.bin"), b"lmgg").unwrap();
        m.refresh();
        assert_eq!(row(&m, "tiny.en").health, ModelHealth::Present);
        m.select("tiny.en");
        assert!(matches!(&m.message, Some(Ok(_))), "{:?}", m.message);
        assert_eq!(m.active, ("whisper".into(), "tiny.en".into()));
        assert!(m.is_active("tiny.en"));
        let cfg = voxtype::config::load_config(Some(&m.config)).unwrap();
        assert_eq!(cfg.whisper.model, "tiny.en");
    }

    #[test]
    fn renders_headless() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = picker(dir.path(), PathBuf::from("voxtype"));
        let ctx = egui::Context::default();
        for engine in model_catalog::CATALOG_ENGINES {
            m.set_engine(engine);
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| m.show(ui));
        }
    }

    /// A stand-in for `voxtype` that runs `body` as a shell script.
    #[cfg(unix)]
    fn fake_voxtype(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-voxtype");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn download_reports_progress_then_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let models = dir.path().join("models");
        let args = dir.path().join("args");
        let script = format!(
            r#"echo "$@" > {args}
echo '{{"event":"progress","model":"tiny.en","file":"ggml-tiny.en.bin","bytes":2,"total":4,"pct":50.0}}'
sleep 0.3
printf lmgg > {models}/ggml-tiny.en.bin
echo '{{"event":"progress","model":"tiny.en","file":"ggml-tiny.en.bin","bytes":4,"total":4,"pct":100.0}}'
echo '{{"event":"done","model":"tiny.en"}}'"#,
            args = args.display(),
            models = models.display()
        );
        let mut m = picker(dir.path(), fake_voxtype(dir.path(), &script));
        m.download("tiny.en");
        let mut seen = None;
        let deadline = Instant::now() + Duration::from_secs(20);
        while m.poll() && Instant::now() < deadline {
            if let Some(p) = m.job.as_ref().and_then(|j| j.progress.clone()) {
                seen = Some(p);
            }
            std::thread::park_timeout(Duration::from_millis(10));
        }
        assert!(m.job.is_none());
        assert_eq!(seen.unwrap().0, "ggml-tiny.en.bin");
        let msg = m.message.clone().unwrap().unwrap();
        assert!(msg.contains("downloaded"), "{msg}");
        assert_eq!(row(&m, "tiny.en").health, ModelHealth::Present);
        assert_eq!(
            std::fs::read_to_string(args).unwrap().trim(),
            "setup --download --model tiny.en --progress-format json --no-post-install"
        );
        // Downloading is not selecting.
        assert_eq!(m.active.1, "base.en");
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_download_shows_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"echo '{"event":"error","model":"tiny.en","message":"HTTP 404"}'; exit 1"#;
        let mut m = picker(dir.path(), fake_voxtype(dir.path(), script));
        m.download("tiny.en");
        finish(&mut m);
        assert_eq!(m.message, Some(Err("whisper / tiny.en: HTTP 404".into())));
        assert_eq!(row(&m, "tiny.en").health, ModelHealth::Missing);
    }

    #[cfg(unix)]
    #[test]
    fn cancel_stops_the_download() {
        let dir = tempfile::tempdir().unwrap();
        let grandchild = dir.path().join("curl.pid");
        // Like the real downloader: a child process (curl) does the work.
        let script = format!(
            r#"sleep 30 & echo $! > {}
echo '{{"event":"progress","model":"tiny.en","file":"f","bytes":1,"total":9,"pct":11.1}}'
wait"#,
            grandchild.display()
        );
        let mut m = picker(dir.path(), fake_voxtype(dir.path(), &script));
        m.download("tiny.en");
        let started = Instant::now();
        while m.job.as_ref().is_some_and(|j| j.progress.is_none()) {
            m.poll();
            std::thread::park_timeout(Duration::from_millis(10));
        }
        m.job.as_ref().unwrap().cancel();
        finish(&mut m);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(matches!(m.message, Some(Err(_))));
        let pid: libc::pid_t = std::fs::read_to_string(grandchild)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // The grandchild is gone (or a zombie awaiting init, which kill(0)
        // still sees; check /proc state where there is one).
        let gone = unsafe { libc::kill(pid, 0) } != 0
            || std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .is_ok_and(|s| s.split_whitespace().nth(2) == Some("Z"));
        assert!(gone, "the downloader's child {pid} survived Cancel");
    }

    /// The real downloader end to end. Network, so opt-in:
    /// `VOXTYPE_APP_REAL_BIN=target/debug/voxtype XDG_DATA_HOME=<tmp>
    /// VOXTYPE_APP_REAL_MODEL=engine/model cargo test -p voxtype-app
    /// real_download -- --ignored`
    #[test]
    #[ignore]
    fn real_download() {
        let bin = std::env::var("VOXTYPE_APP_REAL_BIN").expect("VOXTYPE_APP_REAL_BIN");
        let spec = std::env::var("VOXTYPE_APP_REAL_MODEL").unwrap_or("whisper/tiny.en".into());
        let (engine, model) = spec.split_once('/').unwrap();
        let engine = *model_catalog::CATALOG_ENGINES
            .iter()
            .find(|e| **e == engine)
            .unwrap();
        let model = *model_catalog::model_catalog(engine)
            .iter()
            .find(|m| **m == model)
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut m = Models::new(
            dir.path().join("config.toml"),
            voxtype::config::Config::models_dir(),
            PathBuf::from(bin),
        );
        m.set_engine(engine);
        assert_eq!(row(&m, model).health, ModelHealth::Missing);
        m.download(model);
        let (mut updates, mut last) = (0, None);
        let deadline = Instant::now() + Duration::from_secs(900);
        while m.poll() && Instant::now() < deadline {
            let p = m.job.as_ref().and_then(|j| j.progress.clone());
            if p.is_some() && p != last {
                updates += 1;
                last = p;
            }
            std::thread::park_timeout(Duration::from_millis(50));
        }
        eprintln!("progress updates: {updates}, last: {last:?}");
        eprintln!("result: {:?}", m.message);
        assert!(matches!(m.message, Some(Ok(_))));
        assert!(updates > 1);
        assert_eq!(row(&m, model).health, ModelHealth::Present);
        m.select(model);
        eprintln!("select: {:?}", m.message);
        assert!(m.is_active(model));
    }

    /// The sha256 path on a real R2 model: fetch dolphin-base with the
    /// library's downloader (this build has no dolphin engine, so the CLI
    /// would refuse), then verify it through the picker, and again after
    /// flipping one byte. Network, so opt-in: `XDG_DATA_HOME=<tmp> cargo
    /// test -p voxtype-app real_r2_verify -- --ignored`
    #[test]
    #[ignore]
    fn real_r2_verify() {
        voxtype::setup::model::download_dolphin_model("dolphin-base").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut m = Models::new(
            dir.path().join("config.toml"),
            voxtype::config::Config::models_dir(),
            PathBuf::from("voxtype"),
        );
        m.set_engine("dolphin");
        assert_eq!(row(&m, "dolphin-base").health, ModelHealth::Present);
        m.verify("dolphin-base");
        finish(&mut m);
        eprintln!("verify: {:?}", m.message);
        assert!(matches!(&m.message, Some(Ok(s)) if s.contains("sha256")));
        // Flip one byte in the largest file.
        let model_dir = m.models_dir.join("dolphin-base");
        let biggest = std::fs::read_dir(&model_dir)
            .unwrap()
            .filter_map(Result::ok)
            .max_by_key(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
            .unwrap()
            .path();
        let mut bytes = std::fs::read(&biggest).unwrap();
        bytes[1000] ^= 0xff;
        std::fs::write(&biggest, bytes).unwrap();
        m.verify("dolphin-base");
        finish(&mut m);
        eprintln!("verify after a flipped byte: {:?}", m.message);
        assert!(matches!(&m.message, Some(Err(s)) if s.contains("does not match")));
    }

    #[test]
    fn engines_missing_from_the_build_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = picker(dir.path(), dir.path().join("no-such-voxtype"));
        for engine in model_catalog::CATALOG_ENGINES {
            if voxtype::config_set::engine_feature_compiled(engine) {
                continue;
            }
            m.set_engine(engine);
            assert!(!m.engine_compiled());
            m.download(m.rows[0].name);
            assert!(m.job.is_none());
            assert!(matches!(&m.message, Some(Err(e)) if e.contains("This build has no")));
        }
    }

    #[test]
    fn a_missing_binary_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut m = picker(dir.path(), dir.path().join("no-such-voxtype"));
        m.download("tiny.en");
        assert!(m.job.is_none());
        assert!(matches!(&m.message, Some(Err(e)) if e.contains("could not run")));
    }
}
