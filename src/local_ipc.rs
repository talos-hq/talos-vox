//! Local stream IPC between the daemon and its clients (audio levels, OSD).
//!
//! Unix keeps tokio's pathname sockets. Other hosts route through
//! kernal-api's local IPC, which is a named pipe on Windows; the socket
//! path is used verbatim as the pipe name.

use std::io;
use std::path::Path;

#[cfg(unix)]
pub use tokio::net::UnixStream as Stream;

#[cfg(unix)]
pub struct Listener(tokio::net::UnixListener);

#[cfg(unix)]
impl Listener {
    pub fn bind(path: &Path) -> io::Result<Self> {
        tokio::net::UnixListener::bind(path).map(Self)
    }

    pub async fn accept(&self) -> io::Result<Stream> {
        self.0.accept().await.map(|(stream, _addr)| stream)
    }
}

#[cfg(unix)]
pub async fn connect(path: &Path) -> io::Result<Stream> {
    Stream::connect(path).await
}

#[cfg(not(unix))]
pub use kernal_api::platform::ipc::AsyncStream as Stream;

#[cfg(not(unix))]
fn endpoint(path: &Path) -> io::Result<kernal_api::platform::ipc::Endpoint> {
    kernal_api::platform::ipc::Endpoint::new(path.to_string_lossy().into_owned())
}

#[cfg(not(unix))]
pub struct Listener(kernal_api::platform::ipc::AsyncListener);

#[cfg(not(unix))]
impl Listener {
    pub fn bind(path: &Path) -> io::Result<Self> {
        kernal_api::platform::ipc::AsyncListener::bind(&endpoint(path)?).map(Self)
    }

    pub async fn accept(&self) -> io::Result<Stream> {
        self.0.accept().await
    }
}

#[cfg(not(unix))]
pub async fn connect(path: &Path) -> io::Result<Stream> {
    Stream::connect_within(&endpoint(path)?, std::time::Duration::from_secs(5)).await
}
