//! Shared helper that registers ONNX Runtime execution providers for any
//! ONNX-backed engine in voxtype.
//!
//! Each engine's session builder calls [`register_gpu_eps`] to attach the
//! GPU EPs that were compiled into this binary. The compile-time gating
//! lives on three marker features in `Cargo.toml`:
//!
//! - `onnx-cuda-enabled`     — CUDA EP (NVIDIA)
//! - `onnx-migraphx-enabled` — MIGraphX EP (AMD)
//! - `onnx-tensorrt-enabled` — TensorRT EP (NVIDIA, optimized)
//!
//! Enabling any per-engine feature like `cohere-migraphx` or
//! `parakeet-cuda` transitively enables the matching marker, so this
//! helper sees the right EPs without each engine duplicating the cfg
//! plumbing.
//!
//! Order matters: ort tries EPs in sequence and falls through to the
//! next on registration failure. Specialized EPs (TensorRT) come before
//! their generic siblings (CUDA). The CPU EP is always implicit at the
//! bottom of the chain — even if every GPU EP fails to register at
//! runtime (no GPU, missing driver, missing companion .so files), ort
//! still runs the model on CPU.

#[cfg(feature = "onnx-common")]
use ort::execution_providers::ExecutionProviderDispatch;
#[cfg(feature = "onnx-common")]
use ort::session::builder::{BuilderResult, SessionBuilder};

/// Keep the ONNX Runtime environment alive until the process is gone.
///
/// Must be called before any GPU EP is registered. Registering one loads a
/// shared provider library (`libonnxruntime_providers_<ep>.so`), and from then
/// on releasing the environment during process exit corrupts the heap:
///
/// 1. glibc's `exit` runs every `__cxa_atexit` handler before `_dl_fini`, so
///    the C++ destructor of the provider's static `s_kernel_registry` frees
///    its `shared_ptr` control block without clearing the pointer.
/// 2. `ort` drops its global environment from a `.fini_array` hook, i.e. in
///    `_dl_fini`, which is too late. `ReleaseEnv` reaches
///    `UnloadSharedProviders` -> `DeleteRegistry`, and that second `reset()`
///    decrements the use count at offset 8 of the freed block.
/// 3. Offset 8 of a freed glibc chunk is its `bk` link, so a later free aborts
///    with `corrupted double-linked list` (exit 134), even though the
///    transcript was already printed. See talos-hq/talos-vox#4.
///
/// Leaking one reference means `ort`'s hook drops only its own, `ReleaseEnv`
/// never runs, and the stale `reset()` never happens. The other exit handlers
/// still run; the OS reclaims the environment. `current()` creates the default
/// environment if none exists, which is what session creation would do anyway.
///
/// Remove this once `ort` releases the environment before C++ destructors on
/// Linux.
#[cfg(any(feature = "parakeet", feature = "onnx-common"))]
pub fn keep_ort_environment_until_exit() {
    static PINNED: std::sync::Once = std::sync::Once::new();
    PINNED.call_once(|| match ort::environment::current() {
        Ok(env) => std::mem::forget(env),
        // Session creation will hit and report the same error.
        Err(e) => tracing::warn!("Could not pin ONNX Runtime environment: {e}"),
    });
}

/// Register GPU EPs onto a session builder.
///
/// `engine_label` and `session_label` are used only for logging
/// (`"Cohere encoder: registering execution providers [...]"`). Returns
/// the modified builder; if no EPs are compiled in or registration
/// fails, falls through unchanged and ort uses the CPU EP.
#[cfg(feature = "onnx-common")]
pub fn register_gpu_eps(
    builder: SessionBuilder,
    engine_label: &str,
    session_label: &str,
) -> BuilderResult {
    let providers = compiled_providers();
    if providers.is_empty() {
        return Ok(builder);
    }
    keep_ort_environment_until_exit();
    let names: Vec<&'static str> = providers.iter().map(|(n, _)| *n).collect();
    tracing::info!("{engine_label} {session_label}: registering execution providers {names:?}");
    let dispatches: Vec<_> = providers.into_iter().map(|(_, ep)| ep).collect();
    builder.with_execution_providers(dispatches)
}

#[cfg(feature = "onnx-common")]
fn compiled_providers() -> Vec<(&'static str, ExecutionProviderDispatch)> {
    #[allow(unused_mut)]
    let mut providers: Vec<(&'static str, ExecutionProviderDispatch)> = Vec::new();

    #[cfg(feature = "onnx-tensorrt-enabled")]
    {
        use ort::execution_providers::{ExecutionProvider, TensorRTExecutionProvider};
        providers.push(("TensorRT", TensorRTExecutionProvider::default().build()));
    }
    #[cfg(feature = "onnx-cuda-enabled")]
    {
        use ort::execution_providers::{CUDAExecutionProvider, ExecutionProvider};
        providers.push(("CUDA", CUDAExecutionProvider::default().build()));
    }
    #[cfg(feature = "onnx-migraphx-enabled")]
    {
        use ort::execution_providers::{ExecutionProvider, MIGraphXExecutionProvider};
        providers.push(("MIGraphX", MIGraphXExecutionProvider::default().build()));
    }

    providers
}
