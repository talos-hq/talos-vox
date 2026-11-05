//! OSD launcher. It execs Wayland frontends that exist only on Unix
//! desktops; the implementation lives in `voxtype_osd/unix.rs`.

#[cfg(unix)]
#[path = "voxtype_osd/unix.rs"]
mod unix;

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    unix::main()
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("voxtype-osd is only available on Unix desktops");
    std::process::ExitCode::FAILURE
}
