//! Audio capture through kernal-api's `audio-capture` HAL (ALSA, WASAPI, CoreAudio).
//!
//! kernal-api owns device access and resampling, so this only forwards its 16 kHz mono chunks
//! and keeps the session's samples for `stop`/`get_samples`.

use super::AudioCapture;
use crate::config::AudioConfig;
use crate::error::AudioError;
use kernal_api::audio_capture::{AudioError as HalError, Capture};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub struct KernalCapture {
    device: String,
    max_samples: usize,
    recorded: Arc<Mutex<Vec<f32>>>,
    drained: usize,
    pump: Option<JoinHandle<()>>,
}

impl KernalCapture {
    pub fn new(config: &AudioConfig) -> Result<Self, AudioError> {
        Ok(Self {
            device: config.device.clone(),
            max_samples: config.max_duration_secs as usize * 16_000,
            recorded: Arc::new(Mutex::new(Vec::new())),
            drained: 0,
            pump: None,
        })
    }
}

fn hal_error(err: HalError, device: &str) -> AudioError {
    match err {
        HalError::NoDevice => AudioError::DeviceNotFound(device.to_owned()),
        HalError::Denied => AudioError::Connection("microphone access denied".into()),
        HalError::Unavailable => AudioError::Connection("microphone unavailable".into()),
    }
}

#[async_trait::async_trait]
impl AudioCapture for KernalCapture {
    async fn start(&mut self) -> Result<mpsc::Receiver<Vec<f32>>, AudioError> {
        // kernal-api opens the default input; named-device selection is a HAL follow-up.
        let mut capture = Capture::open_default()
            .await
            .map_err(|e| hal_error(e, &self.device))?;
        let (tx, rx) = mpsc::channel(256);
        self.recorded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.drained = 0;
        let recorded = Arc::clone(&self.recorded);
        let max_samples = self.max_samples;
        self.pump = Some(tokio::spawn(async move {
            while let Some(chunk) = capture.next_chunk_within(Duration::from_secs(5)).await {
                {
                    let mut all = recorded.lock().unwrap_or_else(|e| e.into_inner());
                    if all.len() >= max_samples {
                        break;
                    }
                    all.extend_from_slice(&chunk);
                }
                let _ = tx.try_send(chunk);
            }
        }));
        Ok(rx)
    }

    async fn stop(&mut self) -> Result<Vec<f32>, AudioError> {
        if let Some(pump) = self.pump.take() {
            // Aborting drops the Capture, which stops the device.
            pump.abort();
            let _ = pump.await;
        }
        let samples = std::mem::take(&mut *self.recorded.lock().unwrap_or_else(|e| e.into_inner()));
        if samples.is_empty() {
            return Err(AudioError::EmptyRecording);
        }
        Ok(samples)
    }

    async fn get_samples(&mut self) -> Vec<f32> {
        let all = self.recorded.lock().unwrap_or_else(|e| e.into_inner());
        let fresh = all[self.drained..].to_vec();
        self.drained = all.len();
        fresh
    }
}
