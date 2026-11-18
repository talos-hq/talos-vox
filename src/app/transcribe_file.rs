//! `voxtype transcribe <file>` — one-shot transcription of an audio file.
//!
//! `resample` lives here rather than in `src/audio/` because it has exactly
//! one call site (this command). Per the refactoring policy: don't extract
//! an abstraction from a single use site.

use std::io::Write;
use std::path::Path;
use voxtype::{config, transcribe, vad};

/// Transcribe an audio file.
///
/// stdout carries only the transcript (see [`write_transcript`]); progress
/// lines go to stderr, and `quiet` drops them. Logs are routed to stderr in
/// `main` (`Commands::logs_to_stderr`). Callers such as clud's F3 voice mode
/// capture stdout, so nothing else may be printed there (#4).
pub(crate) fn transcribe_file(
    config: &config::Config,
    path: &Path,
    quiet: bool,
) -> anyhow::Result<()> {
    let outcome = transcribe_path(config, path, &mut |line| {
        if !quiet {
            eprintln!("{}", line);
        }
    })?;
    write_transcript(&mut std::io::stdout().lock(), outcome.text.as_deref())?;
    Ok(())
}

/// The stdout of `voxtype transcribe`: a blank line, then the transcript, or
/// nothing when VAD found no speech.
///
/// The leading blank line is kept from the old format, in which it divided
/// the logs from the transcript: clud's parser (talos-hq/clud#2673) still
/// requires it, so dropping it would make clud find no transcript.
fn write_transcript(out: &mut dyn Write, text: Option<&str>) -> std::io::Result<()> {
    if let Some(text) = text {
        writeln!(out, "\n{}", text)?;
    }
    out.flush()
}

/// What [`transcribe_path`] produced.
pub(crate) struct Outcome {
    /// Processed transcript, or `None` when VAD found no speech.
    pub text: Option<String>,
    /// Length of the 16 kHz mono audio, in seconds. Read by `voxtype mcp`.
    #[cfg_attr(not(feature = "mcp"), allow(dead_code))]
    pub duration_secs: f32,
}

/// Load, resample, VAD-check and transcribe `path`. Progress lines go to
/// `progress` (stderr for the CLI, nowhere for `voxtype mcp`, whose stdout
/// is the protocol); warnings go to stderr.
pub(crate) fn transcribe_path(
    config: &config::Config,
    path: &Path,
    progress: &mut dyn FnMut(String),
) -> anyhow::Result<Outcome> {
    use hound::WavReader;

    progress(format!("Loading audio file: {:?}", path));

    let reader = WavReader::open(path)?;
    let spec = reader.spec();

    progress(format!(
        "Audio format: {} Hz, {} channel(s), {:?}",
        spec.sample_rate, spec.channels, spec.sample_format
    ));

    // Convert samples to f32 mono at 16kHz
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let max_val = (1 << (spec.bits_per_sample - 1)) as f32;
            reader
                .into_samples::<i32>()
                .filter_map(|s| s.ok())
                .map(|s| s as f32 / max_val)
                .collect()
        }
        hound::SampleFormat::Float => reader
            .into_samples::<f32>()
            .filter_map(|s| s.ok())
            .collect(),
    };

    // Mix to mono if stereo
    let mono_samples: Vec<f32> = if spec.channels > 1 {
        samples
            .chunks(spec.channels as usize)
            .map(|chunk| chunk.iter().sum::<f32>() / chunk.len() as f32)
            .collect()
    } else {
        samples
    };

    // Resample to 16kHz if needed
    let final_samples = if spec.sample_rate != 16000 {
        progress(format!(
            "Resampling from {} Hz to 16000 Hz...",
            spec.sample_rate
        ));
        voxtype::audio::resampler::resample_buffer(&mono_samples, spec.sample_rate, 16000)
    } else {
        mono_samples
    };

    let duration_secs = final_samples.len() as f32 / 16000.0;
    progress(format!(
        "Processing {} samples ({:.2}s)...",
        final_samples.len(),
        duration_secs
    ));

    // Run VAD if enabled
    if let Ok(Some(vad)) = vad::create_vad(config) {
        match vad.detect(&final_samples) {
            Ok(result) => {
                progress(format!(
                    "VAD: {:.2}s speech ({:.1}% of audio)",
                    result.speech_duration_secs,
                    result.speech_ratio * 100.0
                ));
                if !result.has_speech {
                    progress("No speech detected, skipping transcription.".to_string());
                    return Ok(Outcome {
                        text: None,
                        duration_secs,
                    });
                }
            }
            Err(e) => {
                eprintln!("VAD warning: {}", e);
                // Continue with transcription if VAD fails
            }
        }
    }

    // Create transcriber and transcribe
    let transcriber = transcribe::create_transcriber(config)?;
    let text = transcriber.transcribe(&final_samples)?;

    Ok(Outcome {
        text: Some(process_transcript(config, &text)),
        duration_secs,
    })
}

/// Apply the configured text pipeline (replacements, spoken punctuation,
/// filler filtering) to the finished transcript — the same treatment the
/// daemon gives a batch dictation before output. `voxtype transcribe`
/// shipped without this, so a config that worked for dictation silently
/// did nothing here (#581).
fn process_transcript(config: &config::Config, text: &str) -> String {
    voxtype::text::TextProcessor::new_for_language(&config.text, config.active_language())
        .process(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #4: stdout is the transcript after one blank line, and nothing else.
    #[test]
    fn stdout_is_blank_line_then_transcript() {
        let mut out = Vec::new();
        write_transcript(&mut out, Some("Hello world.")).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "\nHello world.\n");
    }

    /// #4: no speech means empty stdout, so a caller can't mistake a status
    /// line for a transcript.
    #[test]
    fn stdout_is_empty_without_speech() {
        let mut out = Vec::new();
        write_transcript(&mut out, None).unwrap();
        assert!(out.is_empty());
    }

    /// #581: `voxtype transcribe <file>` must honor [text] replacements and
    /// spoken punctuation like the daemon does.
    #[test]
    fn transcript_gets_replacements_and_spoken_punctuation() {
        let mut config = config::Config::default();
        config
            .text
            .replacements
            .insert("vox type".to_string(), "voxtype".to_string());
        config.text.spoken_punctuation = true;

        assert_eq!(
            process_transcript(&config, "I use vox type period"),
            "I use voxtype."
        );
    }
}
