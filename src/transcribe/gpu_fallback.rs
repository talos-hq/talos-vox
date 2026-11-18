//! CPU fallback for engines whose GPU backend can't initialise.
//!
//! A GPU backend can fail at model load even when the driver and runtime are
//! present: on a nearly full device ONNX Runtime's BFC arena can't allocate,
//! and with `CUDA_VISIBLE_DEVICES=-1` the CUDA EP registers and then fails in
//! `cuda_call.cc`. Rather than exit, Parakeet retries the load on CPU and logs
//! that it did. See talos-hq/talos-vox#4.
//!
//! Whisper does the same. Stock whisper.cpp aborts (`GGML_ASSERT(buffer)`)
//! when the model buffer doesn't fit and segfaults freeing a half-built state
//! when the KV cache doesn't, so voxtype builds a patched copy
//! (talos-hq/whisper-rs-sys, see `[patch.crates-io]` in Cargo.toml) that
//! returns an error to retry on.

use crate::error::TranscribeError;

/// Whether a `CUDA_VISIBLE_DEVICES` value leaves no CUDA device visible.
///
/// CUDA reads the variable as a comma-separated list and stops at the first
/// entry that isn't a device, so an empty value, or one whose first entry is
/// neither a non-negative index nor a `GPU-`/`MIG-` UUID, hides every device.
/// The conventional spelling is `-1`. An unset variable hides nothing.
///
/// An index that parses but is out of range (say `7` on a two-GPU machine)
/// also hides everything, but only the driver knows the device count; that
/// case fails at model load and is caught by [`init_with_cpu_fallback`].
pub fn cuda_devices_hidden(value: Option<&str>) -> bool {
    let Some(value) = value else {
        return false;
    };
    let first = value.split(',').next().unwrap_or_default().trim();
    if first.starts_with("GPU-") || first.starts_with("MIG-") {
        return false;
    }
    first.parse::<u32>().is_err()
}

/// [`cuda_devices_hidden`] for this process's environment.
pub fn cuda_hidden_by_env() -> bool {
    match std::env::var("CUDA_VISIBLE_DEVICES") {
        Ok(value) => cuda_devices_hidden(Some(&value)),
        Err(std::env::VarError::NotPresent) => false,
        // Not UTF-8, so not a device list CUDA can parse either.
        Err(std::env::VarError::NotUnicode(_)) => true,
    }
}

/// Load an engine on GPU, and on CPU if that fails.
///
/// `init(true)` loads on the GPU and `init(false)` on the CPU. With
/// `gpu_available` false the GPU attempt is skipped. When the GPU load fails,
/// a warning naming the error goes to the log (stderr) and the CPU load is
/// tried; if that fails too, the error carries both causes.
pub fn init_with_cpu_fallback<T>(
    engine: &str,
    gpu_available: bool,
    mut init: impl FnMut(bool) -> Result<T, TranscribeError>,
) -> Result<T, TranscribeError> {
    if !gpu_available {
        return init(false);
    }
    let gpu_err = match init(true) {
        Ok(loaded) => return Ok(loaded),
        Err(e) => e,
    };
    tracing::warn!(
        "{engine}: GPU initialisation failed, falling back to CPU: {}",
        detail(&gpu_err)
    );
    match init(false) {
        Ok(loaded) => {
            tracing::warn!("{engine}: running on CPU after the GPU failed to initialise");
            Ok(loaded)
        }
        Err(cpu_err) => Err(TranscribeError::InitFailed(format!(
            "{engine} could not initialise on GPU ({}) or on CPU ({})",
            detail(&gpu_err),
            detail(&cpu_err)
        ))),
    }
}

/// The message inside an `InitFailed`, without its "Transcriber
/// initialization failed:" prefix, so a combined error doesn't repeat it.
fn detail(e: &TranscribeError) -> String {
    match e {
        TranscribeError::InitFailed(msg) => msg.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn unset_variable_hides_nothing() {
        assert!(!cuda_devices_hidden(None));
    }

    #[test]
    fn empty_or_negative_hides_every_device() {
        for value in ["", " ", "-1", "-1,0", "none", ",0"] {
            assert!(cuda_devices_hidden(Some(value)), "{value:?}");
        }
    }

    #[test]
    fn indices_and_uuids_leave_devices_visible() {
        for value in [
            "0",
            "1",
            "0,1",
            " 1 ",
            "1,-1",
            "GPU-8932f937-d72c-4106-c12f-20bd9faed9f6",
            "MIG-GPU-8932f937/1/0",
        ] {
            assert!(!cuda_devices_hidden(Some(value)), "{value:?}");
        }
    }

    #[test]
    fn gpu_success_does_not_touch_cpu() {
        let calls = RefCell::new(Vec::new());
        let result = init_with_cpu_fallback("Test", true, |gpu| {
            calls.borrow_mut().push(gpu);
            Ok::<_, TranscribeError>(if gpu { "gpu" } else { "cpu" })
        });
        assert_eq!(result.unwrap(), "gpu");
        assert_eq!(*calls.borrow(), vec![true]);
    }

    #[test]
    fn gpu_failure_falls_back_to_cpu() {
        let calls = RefCell::new(Vec::new());
        let result = init_with_cpu_fallback("Test", true, |gpu| {
            calls.borrow_mut().push(gpu);
            if gpu {
                Err(TranscribeError::InitFailed(
                    "bfc_arena.cc:358 Failed to allocate memory".into(),
                ))
            } else {
                Ok("cpu")
            }
        });
        assert_eq!(result.unwrap(), "cpu");
        assert_eq!(*calls.borrow(), vec![true, false]);
    }

    #[test]
    fn no_gpu_goes_straight_to_cpu() {
        let calls = RefCell::new(Vec::new());
        let result = init_with_cpu_fallback("Test", false, |gpu| {
            calls.borrow_mut().push(gpu);
            Ok::<_, TranscribeError>(gpu)
        });
        assert!(!result.unwrap());
        assert_eq!(*calls.borrow(), vec![false]);
    }

    #[test]
    fn both_failing_reports_both_causes() {
        let result: Result<(), _> = init_with_cpu_fallback("Test", true, |gpu| {
            Err(TranscribeError::InitFailed(
                if gpu { "gpu broke" } else { "cpu broke" }.into(),
            ))
        });
        let message = result.unwrap_err().to_string();
        assert_eq!(
            message,
            "Transcriber initialization failed: Test could not initialise on GPU \
             (gpu broke) or on CPU (cpu broke)"
        );
    }

    #[test]
    fn cpu_only_failure_is_returned_unchanged() {
        let result: Result<(), _> = init_with_cpu_fallback("Test", false, |_| {
            Err(TranscribeError::ModelNotFound("gone".into()))
        });
        assert!(matches!(result, Err(TranscribeError::ModelNotFound(_))));
    }
}
