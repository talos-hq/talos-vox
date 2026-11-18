//! wlroots compositors without their own probe (river, labwc, Wayfire,
//! niri, ...): the activated toplevel from
//! `zwlr_foreign_toplevel_manager_v1`.
//!
//! This speaks the Wayland wire protocol directly over the compositor
//! socket instead of linking a Wayland client library: the probe needs
//! five messages (`get_registry`, `sync`, `bind`, `sync`, `stop`) and the
//! events those produce, none of which carry file descriptors. That keeps
//! the probe in every build without new dependencies.
//!
//! A compositor that does not advertise the global (GNOME, KDE, cage,
//! anything with the protocol disabled) gives `None` after one round trip,
//! and Modes fall back to the global settings.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Instant;

use super::{AppContext, FocusProbe, PROBE_TIMEOUT};

pub(super) struct Wlr;

impl FocusProbe for Wlr {
    fn name(&self) -> &'static str {
        "wlr-foreign-toplevel"
    }

    fn snapshot(&self) -> Option<AppContext> {
        let path = socket_path(
            std::env::var_os("WAYLAND_DISPLAY").map(PathBuf::from),
            std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
        )?;
        let stream = UnixStream::connect(&path)
            .map_err(|e| tracing::debug!("wlr probe: connect {}: {e}", path.display()))
            .ok()?;
        query(stream)
            .map_err(|e| tracing::debug!("wlr probe: {e}"))
            .ok()?
    }
}

/// `$WAYLAND_DISPLAY` is a socket name under `$XDG_RUNTIME_DIR`, or an
/// absolute path.
fn socket_path(display: Option<PathBuf>, runtime: Option<PathBuf>) -> Option<PathBuf> {
    let display = display.filter(|d| !d.as_os_str().is_empty())?;
    if display.is_absolute() {
        return Some(display);
    }
    Some(runtime?.join(display))
}

const MANAGER: &str = "zwlr_foreign_toplevel_manager_v1";
/// Version 3 adds only the `parent` event, which this ignores.
const MANAGER_MAX_VERSION: u32 = 3;
/// `zwlr_foreign_toplevel_handle_v1.state` value for the focused toplevel.
const STATE_ACTIVATED: u32 = 2;

// Client-allocated object ids, in the order they are created.
const DISPLAY: u32 = 1;
const REGISTRY: u32 = 2;
const SYNC_GLOBALS: u32 = 3;
const MANAGER_ID: u32 = 4;
const SYNC_TOPLEVELS: u32 = 5;

/// Encodes requests in the wire format: a header (object id, then size
/// << 16 | opcode, both native-endian u32), then 32-bit-aligned arguments.
struct Request(Vec<u8>);

impl Request {
    fn new(object: u32, opcode: u16) -> Self {
        let mut buf = Vec::with_capacity(32);
        buf.extend_from_slice(&object.to_ne_bytes());
        buf.extend_from_slice(&u32::from(opcode).to_ne_bytes());
        Self(buf)
    }

    fn uint(mut self, v: u32) -> Self {
        self.0.extend_from_slice(&v.to_ne_bytes());
        self
    }

    fn string(mut self, s: &str) -> Self {
        let len = s.len() + 1; // with the NUL terminator
        self.0.extend_from_slice(&(len as u32).to_ne_bytes());
        self.0.extend_from_slice(s.as_bytes());
        self.0.push(0);
        while !self.0.len().is_multiple_of(4) {
            self.0.push(0);
        }
        self
    }

    fn finish(mut self) -> Vec<u8> {
        let size = self.0.len() as u32;
        let opcode = u32::from_ne_bytes(self.0[4..8].try_into().unwrap());
        self.0[4..8].copy_from_slice(&(size << 16 | opcode).to_ne_bytes());
        self.0
    }
}

/// Reads the arguments of one event.
struct Args<'a>(&'a [u8]);

impl Args<'_> {
    fn uint(&mut self) -> Result<u32, String> {
        if self.0.len() < 4 {
            return Err("truncated event".into());
        }
        let v = u32::from_ne_bytes(self.0[..4].try_into().unwrap());
        self.0 = &self.0[4..];
        Ok(v)
    }

    /// A length-prefixed, padded byte array (strings include their NUL).
    fn array(&mut self) -> Result<&[u8], String> {
        let len = self.uint()? as usize;
        let padded = len.div_ceil(4).saturating_mul(4);
        if self.0.len() < padded {
            return Err("truncated event".into());
        }
        let (data, rest) = self.0.split_at(padded);
        self.0 = rest;
        Ok(&data[..len])
    }

    fn string(&mut self) -> Result<String, String> {
        let raw = self.array()?;
        let raw = raw.strip_suffix(&[0]).unwrap_or(raw);
        Ok(String::from_utf8_lossy(raw).into_owned())
    }
}

#[derive(Default, Clone)]
struct Toplevel {
    title: Option<String>,
    app_id: Option<String>,
    activated: bool,
}

/// Toplevel handles by object id. Events update `pending`; `done` commits
/// it, as the protocol requires.
#[derive(Default)]
struct Toplevels {
    pending: HashMap<u32, Toplevel>,
    current: HashMap<u32, Toplevel>,
    /// Creation order, so the answer is deterministic if a compositor
    /// reports more than one activated toplevel (one per seat).
    order: Vec<u32>,
}

impl Toplevels {
    fn focused(&self) -> Option<AppContext> {
        self.order
            .iter()
            .filter_map(|id| self.current.get(id))
            .find(|t| t.activated)
            .and_then(|t| {
                Some(AppContext {
                    app_id: t.app_id.clone().filter(|a| !a.is_empty())?,
                    title: t.title.clone().filter(|t| !t.is_empty()),
                    source: "wlr-foreign-toplevel",
                })
            })
    }
}

/// One connection's worth of state: the bytes read so far and what they
/// said.
struct Conn<S> {
    stream: S,
    buf: Vec<u8>,
    deadline: Instant,
    manager_global: Option<(u32, u32)>,
    toplevels: Toplevels,
}

impl<S: Read + Write> Conn<S> {
    fn send(&mut self, msg: Vec<u8>) -> Result<(), String> {
        self.stream
            .write_all(&msg)
            .map_err(|e| format!("write: {e}"))
    }

    /// Read and dispatch events until callback `id` fires.
    fn roundtrip(&mut self, id: u32) -> Result<(), String> {
        loop {
            while let Some(len) = self.complete_message() {
                let msg: Vec<u8> = self.buf.drain(..len).collect();
                if self.dispatch(&msg)? == Some(id) {
                    return Ok(());
                }
            }
            if Instant::now() >= self.deadline {
                return Err(format!("no reply within {PROBE_TIMEOUT:?}"));
            }
            let mut chunk = [0u8; 4096];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err("compositor closed the connection".into()),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) => return Err(format!("read: {e}")),
            }
        }
    }

    /// Length of the first buffered message, once all of it has arrived.
    /// A header claiming fewer than 8 bytes yields just the header, which
    /// `dispatch` rejects.
    fn complete_message(&self) -> Option<usize> {
        if self.buf.len() < 8 {
            return None;
        }
        let word = u32::from_ne_bytes(self.buf[4..8].try_into().unwrap());
        let size = ((word >> 16) as usize).max(8);
        (self.buf.len() >= size).then_some(size)
    }

    /// Handle one event. Returns the callback id when it is a
    /// `wl_callback.done`.
    fn dispatch(&mut self, msg: &[u8]) -> Result<Option<u32>, String> {
        let object = u32::from_ne_bytes(msg[0..4].try_into().unwrap());
        let word = u32::from_ne_bytes(msg[4..8].try_into().unwrap());
        let (size, opcode) = ((word >> 16) as usize, word & 0xffff);
        if size < 8 {
            return Err("malformed event header".into());
        }
        let mut args = Args(&msg[8..size]);
        match (object, opcode) {
            // wl_display.error
            (DISPLAY, 0) => {
                let (obj, code) = (args.uint()?, args.uint()?);
                let text = args.string()?;
                Err(format!("protocol error on object {obj} ({code}): {text}"))
            }
            // wl_registry.global
            (REGISTRY, 0) => {
                let name = args.uint()?;
                let interface = args.string()?;
                let version = args.uint()?;
                if interface == MANAGER {
                    self.manager_global = Some((name, version));
                }
                Ok(None)
            }
            // wl_callback.done
            (SYNC_GLOBALS | SYNC_TOPLEVELS, 0) => Ok(Some(object)),
            // zwlr_foreign_toplevel_manager_v1.toplevel
            (MANAGER_ID, 0) => {
                let id = args.uint()?;
                self.toplevels.pending.insert(id, Toplevel::default());
                self.toplevels.order.push(id);
                Ok(None)
            }
            // A handle's events. Anything else (delete_id, registry
            // global_remove, manager finished, output_enter/leave, parent)
            // does not change the answer.
            (id, op) => {
                if let Some(t) = self.toplevels.pending.get_mut(&id) {
                    match op {
                        0 => t.title = Some(args.string()?),
                        1 => t.app_id = Some(args.string()?),
                        4 => {
                            let (states, _) = args.array()?.as_chunks::<4>();
                            t.activated = states
                                .iter()
                                .any(|s| u32::from_ne_bytes(*s) == STATE_ACTIVATED)
                        }
                        5 => {
                            let t = t.clone();
                            self.toplevels.current.insert(id, t);
                        }
                        6 => {
                            self.toplevels.pending.remove(&id);
                            self.toplevels.current.remove(&id);
                        }
                        _ => {}
                    }
                }
                Ok(None)
            }
        }
    }
}

/// Ask the compositor behind `stream` for its activated toplevel.
/// `Ok(None)` when it has no foreign-toplevel manager or nothing is
/// focused.
fn query(stream: UnixStream) -> Result<Option<AppContext>, String> {
    let deadline = Instant::now() + PROBE_TIMEOUT;
    stream
        .set_read_timeout(Some(PROBE_TIMEOUT))
        .and_then(|_| stream.set_write_timeout(Some(PROBE_TIMEOUT)))
        .map_err(|e| e.to_string())?;
    query_on(stream, deadline)
}

fn query_on<S: Read + Write>(stream: S, deadline: Instant) -> Result<Option<AppContext>, String> {
    let mut conn = Conn {
        stream,
        buf: Vec::new(),
        deadline,
        manager_global: None,
        toplevels: Toplevels::default(),
    };
    // wl_display.get_registry, wl_display.sync
    conn.send(Request::new(DISPLAY, 1).uint(REGISTRY).finish())?;
    conn.send(Request::new(DISPLAY, 0).uint(SYNC_GLOBALS).finish())?;
    conn.roundtrip(SYNC_GLOBALS)?;
    let Some((name, version)) = conn.manager_global else {
        tracing::debug!("wlr probe: compositor does not offer {MANAGER}");
        return Ok(None);
    };
    // wl_registry.bind (untyped new_id: interface, version, id). The
    // compositor answers the bind with every current toplevel and its
    // state, so one more sync collects them all.
    conn.send(
        Request::new(REGISTRY, 0)
            .uint(name)
            .string(MANAGER)
            .uint(version.min(MANAGER_MAX_VERSION))
            .uint(MANAGER_ID)
            .finish(),
    )?;
    conn.send(Request::new(DISPLAY, 0).uint(SYNC_TOPLEVELS).finish())?;
    conn.roundtrip(SYNC_TOPLEVELS)?;
    // zwlr_foreign_toplevel_manager_v1.stop; the socket closes right after.
    let _ = conn.send(Request::new(MANAGER_ID, 0).finish());
    Ok(conn.toplevels.focused())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::time::Duration;

    /// Event builder for a scripted compositor.
    fn event(object: u32, opcode: u16) -> Request {
        Request::new(object, opcode)
    }

    fn states(mut r: Request, states: &[u32]) -> Request {
        r.0.extend_from_slice(&((states.len() * 4) as u32).to_ne_bytes());
        for s in states {
            r.0.extend_from_slice(&s.to_ne_bytes());
        }
        r
    }

    /// A fake socket: reads come from a scripted byte stream, writes are
    /// recorded.
    struct Script {
        input: Cursor<Vec<u8>>,
        written: Vec<u8>,
    }

    impl Read for Script {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            // A byte at a time, so partial messages are exercised.
            let n = buf.len().min(1);
            self.input.read(&mut buf[..n])
        }
    }

    impl Write for Script {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn run_script(events: Vec<Vec<u8>>) -> Result<Option<AppContext>, String> {
        let script = Script {
            input: Cursor::new(events.concat()),
            written: Vec::new(),
        };
        query_on(script, Instant::now() + Duration::from_secs(5))
    }

    fn globals(with_manager: bool) -> Vec<Vec<u8>> {
        let mut v = vec![event(REGISTRY, 0)
            .uint(1)
            .string("wl_compositor")
            .uint(6)
            .finish()];
        if with_manager {
            v.push(event(REGISTRY, 0).uint(9).string(MANAGER).uint(3).finish());
        }
        v.push(event(SYNC_GLOBALS, 0).uint(0).finish());
        v.push(event(DISPLAY, 1).uint(SYNC_GLOBALS).finish());
        v
    }

    fn toplevel(id: u32, app_id: &str, title: &str, state: &[u32]) -> Vec<Vec<u8>> {
        vec![
            event(MANAGER_ID, 0).uint(id).finish(),
            event(id, 0).string(title).finish(),
            event(id, 1).string(app_id).finish(),
            states(event(id, 4), state).finish(),
            event(id, 5).finish(),
        ]
    }

    #[test]
    fn finds_the_activated_toplevel() {
        let mut ev = globals(true);
        ev.extend(toplevel(0xff00_0000, "foot", "shell", &[]));
        ev.extend(toplevel(
            0xff00_0001,
            "firefox",
            "Mozilla Firefox",
            &[0, STATE_ACTIVATED],
        ));
        ev.push(event(SYNC_TOPLEVELS, 0).uint(0).finish());
        let app = run_script(ev).unwrap().unwrap();
        assert_eq!(app.app_id, "firefox");
        assert_eq!(app.title.as_deref(), Some("Mozilla Firefox"));
        assert_eq!(app.source, "wlr-foreign-toplevel");
    }

    #[test]
    fn no_manager_global_is_none() {
        assert_eq!(run_script(globals(false)).unwrap(), None);
    }

    #[test]
    fn nothing_activated_is_none() {
        let mut ev = globals(true);
        ev.extend(toplevel(0xff00_0000, "foot", "shell", &[1]));
        ev.push(event(SYNC_TOPLEVELS, 0).uint(0).finish());
        assert_eq!(run_script(ev).unwrap(), None);
    }

    #[test]
    fn state_without_done_is_not_committed() {
        let mut ev = globals(true);
        ev.extend(toplevel(0xff00_0000, "foot", "shell", &[]));
        // Activated, but the compositor has not sent `done` yet.
        ev.push(states(event(0xff00_0000, 4), &[STATE_ACTIVATED]).finish());
        ev.push(event(SYNC_TOPLEVELS, 0).uint(0).finish());
        assert_eq!(run_script(ev).unwrap(), None);
    }

    #[test]
    fn closed_toplevel_is_dropped() {
        let mut ev = globals(true);
        ev.extend(toplevel(0xff00_0000, "foot", "shell", &[STATE_ACTIVATED]));
        ev.push(event(0xff00_0000, 6).finish());
        ev.push(event(SYNC_TOPLEVELS, 0).uint(0).finish());
        assert_eq!(run_script(ev).unwrap(), None);
    }

    #[test]
    fn protocol_error_and_eof_are_errors() {
        let err = vec![event(DISPLAY, 0).uint(2).uint(1).string("bad").finish()];
        assert!(run_script(err).unwrap_err().contains("bad"));
        assert!(run_script(vec![]).is_err());
        // A header claiming a size below its own 8 bytes.
        let mut short = DISPLAY.to_ne_bytes().to_vec();
        short.extend_from_slice(&(4u32 << 16).to_ne_bytes());
        assert!(run_script(vec![short]).unwrap_err().contains("malformed"));
    }

    #[test]
    fn requests_are_well_formed() {
        let mut ev = globals(true);
        ev.push(event(SYNC_TOPLEVELS, 0).uint(0).finish());
        let mut script = Script {
            input: Cursor::new(ev.concat()),
            written: Vec::new(),
        };
        query_on(&mut script, Instant::now() + Duration::from_secs(5)).unwrap();
        let w = &script.written;
        let word = |i: usize| u32::from_ne_bytes(w[i..i + 4].try_into().unwrap());
        // get_registry: object 1, 12 bytes, opcode 1, new id 2.
        assert_eq!(
            (word(0), word(4), word(8)),
            (DISPLAY, 12 << 16 | 1, REGISTRY)
        );
        // sync: object 1, opcode 0, new id 3.
        assert_eq!(
            (word(12), word(16), word(20)),
            (DISPLAY, 12 << 16, SYNC_GLOBALS)
        );
        // bind: name 9, "zwlr_foreign_toplevel_manager_v1\0" (33 bytes,
        // padded to 36), version 3, id 4 => 8 + 4 + 4 + 36 + 4 + 4 = 60.
        assert_eq!((word(24), word(28), word(32)), (REGISTRY, 60 << 16, 9));
        assert_eq!(word(36), 33);
        assert_eq!(&w[40..72], MANAGER.as_bytes());
        assert_eq!((word(76), word(80)), (3, MANAGER_ID));
    }

    #[test]
    fn socket_path_resolution() {
        let rt = Some(PathBuf::from("/run/user/1000"));
        assert_eq!(
            socket_path(Some("wayland-1".into()), rt.clone()),
            Some(PathBuf::from("/run/user/1000/wayland-1"))
        );
        assert_eq!(
            socket_path(Some("/tmp/w".into()), None),
            Some(PathBuf::from("/tmp/w"))
        );
        assert_eq!(socket_path(Some("wayland-1".into()), None), None);
        assert_eq!(socket_path(None, rt.clone()), None);
        assert_eq!(socket_path(Some("".into()), rt), None);
    }
}
