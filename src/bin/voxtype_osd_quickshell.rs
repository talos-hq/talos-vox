//! Quickshell OSD launcher. Quickshell exists only on Unix desktops; the
//! implementation lives in `voxtype_osd_quickshell/unix.rs`.

#[cfg(unix)]
#[path = "voxtype_osd_quickshell/unix.rs"]
mod unix;

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    unix::main()
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("voxtype-osd-quickshell is only available on Unix desktops");
    std::process::ExitCode::FAILURE
}
