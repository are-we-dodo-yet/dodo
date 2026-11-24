// Copyright 2018-2026 the Deno authors. MIT license.

//! The permission broker client (`DENO_PERMISSION_BROKER_PATH`).
//!
//! # Protocol
//!
//! Deno connects to the broker's Unix socket (a named pipe on Windows) and
//! sends one request per line (JSON, `\n`-terminated); the broker answers
//! each with one line. One request is outstanding at a time.
//!
//! ## v1 (unchanged)
//!
//! Request: `{"v":1,"pid":N,"id":N,"datetime":"...","permission":"read",
//! "value":"/some/path"}` (`value` may be `null`). Response:
//! `{"id":N,"result":"allow"|"deny","reason":"..."}`. Deno then performs the
//! operation itself. A v1 broker never sees anything else.
//!
//! ## v2: the broker opens the file and hands back the descriptor
//!
//! A broker declares v2 by putting `"v":2` in its **first** response; absent
//! (or lower), the connection stays v1 for its whole life. v2 adds one
//! request, sent only on Unix and only after the declaration, when a file is
//! opened by path (`Deno.open`, `Deno.readFile`, `Deno.writeFile`, node:fs'
//! opens, ...), after the permission check:
//!
//! ```text
//! {"v":2,"pid":N,"id":N,"datetime":"...","permission":"open",
//!  "value":"/absolute/path","open":{"read":true,"write":false,
//!  "create":false,"createNew":false,"truncate":false,"append":false,
//!  "mode":null,"customFlags":null}}
//! ```
//!
//! Only opens by path are sent. Other operations on a path (copying a file,
//! truncating, `stat`, renaming, removing, ...) keep v1's yes / no answer, as
//! do paths that name the opener's own objects (`/dev/tty`,
//! `/dev/std{in,out,err}`, `/dev/fd/*` and, on Linux, `/proc/**`, also through
//! a directory link), which a broker would resolve to its own: Deno opens
//! those itself after the permission check.
//!
//! The broker answers with one of:
//! - `{"id":N,"result":"allow","fd":true}`, sending exactly one open
//!   descriptor as `SCM_RIGHTS` ancillary data with the line's bytes. Deno
//!   uses that descriptor instead of opening the path, which closes the window
//!   between the decision and the open (the broker should open with
//!   `O_NOFOLLOW` / `openat2(RESOLVE_BENEATH)` style flags so the path it
//!   decided on is the file it opened);
//! - `{"id":N,"result":"allow"}`: no descriptor; Deno opens the path itself,
//!   as under v1;
//! - `{"id":N,"result":"deny","reason":"..."}`: the open fails.
//!
//! Deno checks a received descriptor before using it and refuses it (the
//! open fails, the connection stays up) unless: it is not a socket or a
//! symlink (and on Linux not an `O_PATH` descriptor); a directory only for a
//! read without write or create; its access mode (`fcntl(F_GETFL)`) is the
//! requested one (`O_RDONLY` / `O_WRONLY` / `O_RDWR`, with `O_APPEND` for an
//! append); and its path (`/proc/self/fd/N` on Linux, `F_GETPATH` on macOS;
//! other systems can't tell, so they refuse) is the requested path, its
//! parent directory resolved. A file renamed or unlinked after the broker
//! opened it no longer matches and is refused.
//!
//! ## Framing rules (both versions)
//!
//! A response line is at most [`MAX_RESPONSE_BYTES`]; it must be the only
//! thing the broker sent for the request (no bytes after its `\n`); a v1
//! response, or a v2 response without `"fd":true`, carries no descriptor, and
//! one with `"fd":true` carries exactly one. Anything else (and a response
//! that isn't valid JSON, has the wrong `id` or an unknown `result`) is a
//! malformed frame: Deno closes the connection and exits with code 87, as it
//! does for any broker failure.
//!
//! Windows: the client speaks v1 only. A broker's `"v":2` is ignored there,
//! so no open request is sent and no handle is ever handed over (that would
//! need the broker to `DuplicateHandle` into this process, which isn't
//! implemented); the window between the decision and the open remains.

use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;

use parking_lot::Mutex;

use super::BrokerResponse;
use crate::ipc_pipe::IpcPipe;

// TODO(bartlomieju): currently randomly selected exit code, it should
// be documented
static BROKER_EXIT_CODE: i32 = 87;

/// The longest response line the client accepts, in bytes.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024;

static PERMISSION_BROKER: OnceLock<PermissionBroker> = OnceLock::new();
static PID: OnceLock<u32> = OnceLock::new();

pub fn set_broker(broker: PermissionBroker) {
  assert!(PERMISSION_BROKER.set(broker).is_ok());
  assert!(PID.set(std::process::id()).is_ok());
}

pub fn has_broker() -> bool {
  PERMISSION_BROKER.get().is_some()
}

fn current_pid() -> u32 {
  PID.get().copied().unwrap_or_else(std::process::id)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PermissionBrokerRequest<'a> {
  v: u32,
  pid: u32,
  id: u32,
  datetime: String,
  permission: &'a str,
  value: Option<String>,
}

#[cfg(unix)]
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PermissionBrokerOpenRequest<'a> {
  v: u32,
  pid: u32,
  id: u32,
  datetime: String,
  permission: &'static str,
  value: &'a str,
  open: &'a BrokerOpenOptions,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PermissionBrokerResponse {
  id: u32,
  result: String,
  reason: Option<String>,
  /// The protocol version the broker speaks; read from its first response.
  #[serde(default)]
  v: Option<u32>,
  /// v2: a descriptor came with this response.
  #[serde(default)]
  fd: bool,
}

/// How a file is to be opened, as sent in a v2 `open` request.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrokerOpenOptions {
  pub read: bool,
  pub write: bool,
  pub create: bool,
  pub create_new: bool,
  pub truncate: bool,
  pub append: bool,
  pub mode: Option<u32>,
  pub custom_flags: Option<i32>,
}

/// The broker's answer to a v2 `open` request.
#[derive(Debug)]
pub enum BrokerOpenOutcome {
  /// The broker opened the file; use this instead of opening the path.
  File(std::fs::File),
  /// Allowed without a descriptor: open the path as usual.
  OpenItself,
}

/// A response line and the descriptors that came with it.
struct Frame {
  line: Vec<u8>,
  #[cfg(unix)]
  fds: Vec<std::os::fd::OwnedFd>,
}

impl Frame {
  fn has_fds(&self) -> bool {
    #[cfg(unix)]
    {
      !self.fds.is_empty()
    }
    #[cfg(not(unix))]
    {
      false
    }
  }
}

fn malformed(msg: &str) -> std::io::Error {
  std::io::Error::new(
    std::io::ErrorKind::InvalidData,
    format!("Permission broker sent a malformed response: {msg}"),
  )
}

pub struct PermissionBroker {
  stream: Mutex<IpcPipe>,
  next_id: AtomicU32,
  /// Whether the first response has been read (it decides the version).
  negotiated: AtomicBool,
  /// The broker declared v2 in its first response (Unix only).
  v2: AtomicBool,
}

impl PermissionBroker {
  pub fn new(socket_path: impl Into<PathBuf>) -> Self {
    let socket_path = socket_path.into();
    let stream = match IpcPipe::connect(&socket_path) {
      Ok(s) => s,
      Err(err) => {
        log::error!("Failed to create permission broker: {:?}", err);
        std::process::exit(BROKER_EXIT_CODE);
      }
    };
    Self::from_pipe(stream)
  }

  fn from_pipe(stream: IpcPipe) -> Self {
    Self {
      stream: Mutex::new(stream),
      next_id: AtomicU32::new(1),
      negotiated: AtomicBool::new(false),
      v2: AtomicBool::new(false),
    }
  }

  /// Whether the broker speaks v2 (and so takes `open` requests).
  pub fn supports_open(&self) -> bool {
    self.v2.load(Ordering::Acquire)
  }

  /// Sends one request line and reads its response.
  fn exchange(
    &self,
    stream: &mut IpcPipe,
    id: u32,
    request_line: &str,
  ) -> std::io::Result<(PermissionBrokerResponse, Frame)> {
    log::trace!("-> broker req   {}", request_line);
    stream.write_all(request_line.as_bytes())?;
    #[cfg(unix)]
    let frame = {
      let (line, fds) = stream.recv_line(MAX_RESPONSE_BYTES)?;
      Frame { line, fds }
    };
    #[cfg(not(unix))]
    let frame = Frame {
      line: stream.recv_line(MAX_RESPONSE_BYTES)?,
    };
    let response = serde_json::from_slice::<PermissionBrokerResponse>(
      trim_ascii_end(&frame.line),
    )
    .map_err(|err| malformed(&err.to_string()))?;
    log::trace!("<- broker resp  {:?}", response);
    if response.id != id {
      return Err(std::io::Error::other(
        "Permission broker response ID mismatch",
      ));
    }
    if !self.negotiated.swap(true, Ordering::AcqRel)
      && response.v.is_some_and(|v| v >= 2)
      && cfg!(unix)
    {
      self.v2.store(true, Ordering::Release);
    }
    Ok((response, frame))
  }

  fn next_id(&self) -> u32 {
    self.next_id.fetch_add(1, Ordering::Relaxed)
  }

  fn check(
    &self,
    permission: &str,
    stringified_value: Option<String>,
  ) -> std::io::Result<BrokerResponse> {
    let mut stream = self.stream.lock();
    let id = self.next_id();
    let request = PermissionBrokerRequest {
      v: 1,
      pid: current_pid(),
      id,
      #[allow(clippy::disallowed_methods, reason = "TODO: use sys_traits")]
      datetime: chrono::Utc::now().to_rfc3339(),
      permission,
      value: stringified_value,
    };
    let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
    let (response, frame) = self.exchange(&mut stream, id, &msg)?;
    if response.fd || frame.has_fds() {
      return Err(malformed("a descriptor in answer to a permission check"));
    }
    match response.result.as_str() {
      "allow" => Ok(BrokerResponse::Allow),
      "deny" => Ok(BrokerResponse::Deny {
        message: response.reason,
      }),
      _ => Err(std::io::Error::other(
        "Permission broker unknown result variant",
      )),
    }
  }

  /// Sends a v2 `open` request. The outer error is a protocol failure (the
  /// connection is unusable); the inner one refuses this open only.
  #[cfg(unix)]
  #[allow(
    clippy::disallowed_methods,
    reason = "checks a descriptor against the real file system, which sys_traits doesn't abstract"
  )]
  fn open(
    &self,
    path: &Path,
    options: &BrokerOpenOptions,
  ) -> std::io::Result<std::io::Result<BrokerOpenOutcome>> {
    // The broker runs in another directory: name the file absolutely.
    let path = match std::path::absolute(path) {
      Ok(path) => deno_path_util::normalize_path(std::borrow::Cow::Owned(path))
        .into_owned(),
      Err(err) => return Ok(Err(err)),
    };
    let path = path.as_path();
    if opens_locally(path) {
      // Already decided by the permission check; the broker would open its
      // own object under this name.
      return Ok(Ok(BrokerOpenOutcome::OpenItself));
    }
    let Some(value) = path.to_str() else {
      // Not representable in the JSON request: the broker can't name it, so
      // it can't open it either.
      return Ok(Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "the permission broker can't open a path that isn't valid UTF-8",
      )));
    };
    let mut stream = self.stream.lock();
    let id = self.next_id();
    let request = PermissionBrokerOpenRequest {
      v: 2,
      pid: current_pid(),
      id,
      #[allow(clippy::disallowed_methods, reason = "TODO: use sys_traits")]
      datetime: chrono::Utc::now().to_rfc3339(),
      permission: "open",
      value,
      open: options,
    };
    let msg = format!("{}\n", serde_json::to_string(&request).unwrap());
    let (response, mut frame) = self.exchange(&mut stream, id, &msg)?;
    drop(stream);
    match (response.result.as_str(), response.fd) {
      ("allow", true) => {
        if frame.fds.len() != 1 {
          return Err(malformed(&format!(
            "\"fd\":true with {} descriptors",
            frame.fds.len()
          )));
        }
        let fd = frame.fds.pop().unwrap();
        Ok(
          validate_fd(&fd, path, options)
            .map(|()| BrokerOpenOutcome::File(std::fs::File::from(fd))),
        )
      }
      (_, true) => Err(malformed("\"fd\":true on a refusal")),
      _ if frame.has_fds() => {
        Err(malformed("a descriptor without \"fd\":true"))
      }
      ("allow", false) => Ok(Ok(BrokerOpenOutcome::OpenItself)),
      ("deny", false) => Ok(Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        match response.reason {
          Some(reason) => {
            format!("The permission broker refused to open {value}: {reason}")
          }
          None => format!("The permission broker refused to open {value}"),
        },
      ))),
      _ => Err(std::io::Error::other(
        "Permission broker unknown result variant",
      )),
    }
  }
}

/// Whether `path` names an object of whichever process opens it: the
/// controlling terminal (`/dev/tty`), the standard streams and descriptors
/// (`/dev/std{in,out,err}`, `/dev/fd/*`) and, on Linux, `/proc` (`self`,
/// `thread-self` and pid numbers are relative to the opener's process and pid
/// namespace). A broker would open its own, so these are never sent in an
/// `open` request: the permission check has already answered yes or no, and
/// the caller opens the path itself, as under v1. None of these names can be a
/// link someone else planted (devtmpfs's fixed nodes; procfs links are made
/// by the kernel), so the race v2 exists to close doesn't arise for them.
///
/// Both the path as written and the path with its parent directory resolved
/// (what the kernel reaches, see [`expected_fd_path`]) are considered: a
/// directory link to `/dev` or `/proc` opens locally too.
#[cfg(unix)]
fn opens_locally(path: &Path) -> bool {
  fn special(path: &Path) -> bool {
    path == Path::new("/dev/tty")
      || path == Path::new("/dev/stdin")
      || path == Path::new("/dev/stdout")
      || path == Path::new("/dev/stderr")
      || path.starts_with("/dev/fd")
      || cfg!(any(target_os = "linux", target_os = "android"))
        && path.starts_with("/proc")
  }
  special(path) || expected_fd_path(path).is_ok_and(|p| special(&p))
}

fn trim_ascii_end(bytes: &[u8]) -> &[u8] {
  let mut end = bytes.len();
  while end > 0 && bytes[end - 1].is_ascii_whitespace() {
    end -= 1;
  }
  &bytes[..end]
}

/// Checks that a descriptor the broker sent is the file that was asked for,
/// opened the way it was asked for (see the module documentation).
#[cfg(unix)]
fn validate_fd(
  fd: &std::os::fd::OwnedFd,
  path: &Path,
  options: &BrokerOpenOptions,
) -> std::io::Result<()> {
  use std::os::fd::AsRawFd;

  let refuse = |why: &str| {
    std::io::Error::new(
      std::io::ErrorKind::PermissionDenied,
      format!(
        "The permission broker sent a descriptor that doesn't match the \
         request for {}: {why}",
        path.display()
      ),
    )
  };
  let raw = fd.as_raw_fd();

  // SAFETY: fstat into a zeroed stat on a descriptor we own
  let st = unsafe {
    let mut st: libc::stat = std::mem::zeroed();
    if libc::fstat(raw, &mut st) != 0 {
      return Err(refuse(&std::io::Error::last_os_error().to_string()));
    }
    st
  };
  let kind = st.st_mode & libc::S_IFMT;
  let writes = options.write || options.append;
  match kind {
    libc::S_IFSOCK => return Err(refuse("it is a socket")),
    libc::S_IFLNK => return Err(refuse("it is a symlink")),
    libc::S_IFDIR if writes || options.create || options.create_new => {
      return Err(refuse("it is a directory"));
    }
    _ => {}
  }

  // SAFETY: F_GETFL on a descriptor we own
  let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
  if flags < 0 {
    return Err(refuse(&std::io::Error::last_os_error().to_string()));
  }
  #[cfg(any(target_os = "linux", target_os = "android"))]
  if flags & libc::O_PATH != 0 {
    return Err(refuse("it is an O_PATH descriptor"));
  }
  let expected_mode = match (options.read, writes) {
    (_, false) => libc::O_RDONLY,
    (false, true) => libc::O_WRONLY,
    (true, true) => libc::O_RDWR,
  };
  if flags & libc::O_ACCMODE != expected_mode {
    return Err(refuse("its access mode is not the requested one"));
  }
  if options.append && flags & libc::O_APPEND == 0 {
    return Err(refuse("it is not open for appending"));
  }

  let actual = fd_path(raw)
    .map_err(|err| refuse(&format!("its path can't be read back ({err})")))?;
  let expected = expected_fd_path(path)
    .map_err(|err| refuse(&format!("the requested path ({err})")))?;
  if !same_path(&actual, &expected) {
    // The broker's file name stays out of the error, which the program sees.
    log::debug!(
      "permission broker descriptor for {} is open on {}",
      path.display(),
      actual.display()
    );
    return Err(refuse("it is open on another file"));
  }
  Ok(())
}

/// The path a descriptor is open on, as the kernel reports it.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[allow(
  clippy::disallowed_methods,
  reason = "checks a descriptor against the real file system, which sys_traits doesn't abstract"
)]
fn fd_path(raw: std::os::fd::RawFd) -> std::io::Result<PathBuf> {
  std::fs::read_link(format!("/proc/self/fd/{raw}"))
}

#[cfg(target_os = "macos")]
fn fd_path(raw: std::os::fd::RawFd) -> std::io::Result<PathBuf> {
  use std::os::unix::ffi::OsStrExt;
  let mut buf = vec![0u8; libc::PATH_MAX as usize];
  // SAFETY: F_GETPATH writes at most PATH_MAX bytes into buf
  let res = unsafe { libc::fcntl(raw, libc::F_GETPATH, buf.as_mut_ptr()) };
  if res < 0 {
    return Err(std::io::Error::last_os_error());
  }
  let len = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
  Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len])))
}

#[cfg(all(
  unix,
  not(any(target_os = "linux", target_os = "android", target_os = "macos"))
))]
fn fd_path(_raw: std::os::fd::RawFd) -> std::io::Result<PathBuf> {
  Err(std::io::Error::new(
    std::io::ErrorKind::Unsupported,
    "this system can't report a descriptor's path",
  ))
}

/// The path a descriptor opened on `path` reports: `path` with its parent
/// directory resolved (the last component is the file itself, which the
/// broker opened without following a link).
#[cfg(unix)]
#[allow(
  clippy::disallowed_methods,
  reason = "checks a descriptor against the real file system, which sys_traits doesn't abstract"
)]
fn expected_fd_path(path: &Path) -> std::io::Result<PathBuf> {
  let path = std::path::absolute(path)?;
  let path =
    deno_path_util::normalize_path(std::borrow::Cow::Owned(path)).into_owned();
  match (path.parent(), path.file_name()) {
    (Some(parent), Some(name)) => Ok(std::fs::canonicalize(parent)?.join(name)),
    _ => std::fs::canonicalize(&path),
  }
}

#[cfg(unix)]
fn same_path(actual: &Path, expected: &Path) -> bool {
  if actual == expected {
    return true;
  }
  // macOS file systems are usually case-insensitive, and F_GETPATH reports
  // the name as stored: compare the last component without case there, the
  // directories (resolved on both sides) exactly.
  #[cfg(target_os = "macos")]
  {
    if let (Some(a_parent), Some(e_parent), Some(a_name), Some(e_name)) = (
      actual.parent(),
      expected.parent(),
      actual.file_name().and_then(|n| n.to_str()),
      expected.file_name().and_then(|n| n.to_str()),
    ) {
      return a_parent == e_parent && a_name.eq_ignore_ascii_case(e_name);
    }
  }
  false
}

pub fn maybe_check_with_broker(
  name: &str,
  stringified_value_fn: impl Fn() -> Option<String>,
) -> Option<BrokerResponse> {
  let broker = PERMISSION_BROKER.get()?;

  let resp = match broker.check(name, stringified_value_fn()) {
    Ok(resp) => resp,
    Err(err) => {
      log::error!("{:?}", err);
      std::process::exit(BROKER_EXIT_CODE);
    }
  };
  Some(resp)
}

/// Asks a v2 broker to open `path` (see the module documentation). `None`
/// when there is no broker or it speaks v1: the caller opens the path itself.
/// `Some(Err(..))` refuses the open. A protocol failure exits the process
/// with code 87, as for a permission check.
pub fn maybe_open_with_broker(
  path: &Path,
  options: &BrokerOpenOptions,
) -> Option<std::io::Result<BrokerOpenOutcome>> {
  let broker = PERMISSION_BROKER.get()?;
  open_with(broker, path, options)
}

#[cfg(unix)]
fn open_with(
  broker: &PermissionBroker,
  path: &Path,
  options: &BrokerOpenOptions,
) -> Option<std::io::Result<BrokerOpenOutcome>> {
  if !broker.supports_open() {
    return None;
  }
  match broker.open(path, options) {
    Ok(result) => Some(result),
    Err(err) => {
      log::error!("{:?}", err);
      std::process::exit(BROKER_EXIT_CODE);
    }
  }
}

#[cfg(not(unix))]
fn open_with(
  _broker: &PermissionBroker,
  _path: &Path,
  _options: &BrokerOpenOptions,
) -> Option<std::io::Result<BrokerOpenOutcome>> {
  None
}

#[cfg(all(test, unix))]
#[allow(clippy::disallowed_methods, reason = "the tests use real files")]
mod tests {
  use std::io::BufRead;
  use std::io::BufReader;
  use std::os::fd::AsRawFd;
  use std::os::unix::net::UnixStream;
  use std::sync::mpsc;

  use super::*;

  /// Sends `line` (plus `\n`) with `fds` attached as `SCM_RIGHTS`.
  fn send_with_fds(sock: &UnixStream, line: &str, fds: &[i32]) {
    let mut data = line.as_bytes().to_vec();
    data.push(b'\n');
    let mut iov = libc::iovec {
      iov_base: data.as_mut_ptr() as *mut libc::c_void,
      iov_len: data.len(),
    };
    let mut control = [0u64; 16];
    // SAFETY: a msghdr over stack buffers, filled through the CMSG macros
    let res = unsafe {
      let mut msg: libc::msghdr = std::mem::zeroed();
      msg.msg_iov = &mut iov;
      msg.msg_iovlen = 1;
      if !fds.is_empty() {
        let size = std::mem::size_of_val(fds) as u32;
        msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = libc::CMSG_SPACE(size) as _;
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(size) as _;
        std::ptr::copy_nonoverlapping(
          fds.as_ptr(),
          libc::CMSG_DATA(cmsg) as *mut i32,
          fds.len(),
        );
      }
      libc::sendmsg(sock.as_raw_fd(), &msg, 0)
    };
    assert!(res >= 0, "{}", std::io::Error::last_os_error());
  }

  /// A fake broker on the other end of a socket pair: `answer` gets each
  /// request (parsed) and returns the response line plus descriptors to
  /// send. The requests' raw lines come back on the channel.
  fn fake_broker(
    mut answer: impl FnMut(&serde_json::Value) -> (String, Vec<std::fs::File>)
    + Send
    + 'static,
  ) -> (PermissionBroker, mpsc::Receiver<String>) {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
      let mut reader = BufReader::new(theirs.try_clone().unwrap());
      loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
          return;
        }
        let _ = tx.send(line.clone());
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        let (response, files) = answer(&request);
        let fds: Vec<i32> = files.iter().map(|f| f.as_raw_fd()).collect();
        send_with_fds(&theirs, &response, &fds);
      }
    });
    (
      PermissionBroker::from_pipe(IpcPipe::from_unix_stream(ours)),
      rx,
    )
  }

  fn read_only() -> BrokerOpenOptions {
    BrokerOpenOptions {
      read: true,
      ..Default::default()
    }
  }

  fn open_nofollow(path: &Path) -> std::fs::File {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
      .read(true)
      .custom_flags(libc::O_NOFOLLOW)
      .open(path)
      .unwrap()
  }

  #[test]
  fn v1_requests_are_unchanged_and_never_open() {
    let (broker, rx) = fake_broker(|req| {
      let id = req["id"].as_u64().unwrap();
      if req["permission"] == "env" {
        (
          format!(r#"{{"id":{id},"result":"deny","reason":"no"}}"#),
          vec![],
        )
      } else {
        (format!(r#"{{"id":{id},"result":"allow"}}"#), vec![])
      }
    });
    assert_eq!(
      broker.check("read", Some("/a".into())).unwrap(),
      BrokerResponse::Allow
    );
    assert_eq!(
      broker.check("env", None).unwrap(),
      BrokerResponse::Deny {
        message: Some("no".into())
      }
    );
    assert!(!broker.supports_open());
    let first = rx.recv().unwrap();
    let first: serde_json::Value = serde_json::from_str(&first).unwrap();
    // Exactly the v1 fields, in v1 form.
    let keys: Vec<&str> = first
      .as_object()
      .unwrap()
      .keys()
      .map(|k| k.as_str())
      .collect();
    assert_eq!(keys.len(), 6, "{keys:?}");
    assert_eq!(first["v"], 1);
    assert_eq!(first["permission"], "read");
    assert_eq!(first["value"], "/a");
  }

  #[test]
  fn v2_is_declared_by_the_first_answer_only() {
    let (broker, _rx) = fake_broker(|req| {
      let id = req["id"].as_u64().unwrap();
      let v = if id == 1 { "" } else { r#","v":2"# };
      (format!(r#"{{"id":{id},"result":"allow"{v}}}"#), vec![])
    });
    broker.check("read", Some("/a".into())).unwrap();
    broker.check("read", Some("/a".into())).unwrap();
    assert!(!broker.supports_open());

    let (broker, _rx) = fake_broker(|req| {
      let id = req["id"].as_u64().unwrap();
      (format!(r#"{{"id":{id},"result":"allow","v":2}}"#), vec![])
    });
    broker.check("read", Some("/a".into())).unwrap();
    assert!(broker.supports_open());
  }

  fn v2_broker(
    on_open: impl FnMut(&serde_json::Value) -> (String, Vec<std::fs::File>)
    + Send
    + 'static,
  ) -> PermissionBroker {
    let mut on_open = on_open;
    let (broker, _rx) = fake_broker(move |req| {
      if req["permission"] == "open" {
        on_open(req)
      } else {
        let id = req["id"].as_u64().unwrap();
        (format!(r#"{{"id":{id},"result":"allow","v":2}}"#), vec![])
      }
    });
    broker.check("read", Some("/".into())).unwrap();
    assert!(broker.supports_open());
    broker
  }

  #[test]
  fn v2_open_hands_back_the_brokers_descriptor() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data.txt");
    std::fs::write(&file, "public").unwrap();
    let seen = std::sync::Arc::new(Mutex::new(None));
    let seen2 = seen.clone();
    let broker = v2_broker(move |req| {
      *seen2.lock() = Some(req.clone());
      let id = req["id"].as_u64().unwrap();
      let path = PathBuf::from(req["value"].as_str().unwrap());
      (
        format!(r#"{{"id":{id},"result":"allow","fd":true}}"#),
        vec![open_nofollow(&path)],
      )
    });
    let outcome = broker.open(&file, &read_only()).unwrap().unwrap();
    let BrokerOpenOutcome::File(mut f) = outcome else {
      panic!("no descriptor");
    };
    let mut text = String::new();
    std::io::Read::read_to_string(&mut f, &mut text).unwrap();
    assert_eq!(text, "public");
    let req = seen.lock().clone().unwrap();
    assert_eq!(req["v"], 2);
    assert_eq!(req["open"]["read"], true);
    assert_eq!(req["open"]["write"], false);
    assert_eq!(req["value"], file.to_str().unwrap());
  }

  #[test]
  fn v2_allow_without_fd_and_deny() {
    let broker = v2_broker(|req| {
      let id = req["id"].as_u64().unwrap();
      if req["value"].as_str().unwrap().ends_with("ok") {
        (format!(r#"{{"id":{id},"result":"allow"}}"#), vec![])
      } else {
        (
          format!(r#"{{"id":{id},"result":"deny","reason":"nope"}}"#),
          vec![],
        )
      }
    });
    assert!(matches!(
      broker.open(Path::new("/tmp/ok"), &read_only()).unwrap(),
      Ok(BrokerOpenOutcome::OpenItself)
    ));
    let err = broker
      .open(Path::new("/tmp/no"), &read_only())
      .unwrap()
      .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(err.to_string().contains("nope"), "{err}");
  }

  #[test]
  fn v2_refuses_a_descriptor_for_another_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data.txt");
    let other = dir.path().join("secret.txt");
    std::fs::write(&file, "public").unwrap();
    std::fs::write(&other, "secret").unwrap();
    let other2 = other.clone();
    let broker = v2_broker(move |req| {
      let id = req["id"].as_u64().unwrap();
      (
        format!(r#"{{"id":{id},"result":"allow","fd":true}}"#),
        vec![open_nofollow(&other2)],
      )
    });
    let err = broker.open(&file, &read_only()).unwrap().unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(err.to_string().contains("doesn't match"), "{err}");
    // The file the broker opened isn't named to the program.
    assert!(!err.to_string().contains("secret"), "{err}");
  }

  #[test]
  fn v2_never_sends_paths_that_name_the_openers_own_objects() {
    let dir = tempfile::tempdir().unwrap();
    let dev_link = dir.path().join("dev");
    std::os::unix::fs::symlink("/dev", &dev_link).unwrap();
    let file = dir.path().join("data.txt");
    std::fs::write(&file, "public").unwrap();
    let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
    let sent2 = sent.clone();
    let broker = v2_broker(move |req| {
      sent2
        .lock()
        .push(req["value"].as_str().unwrap().to_string());
      let id = req["id"].as_u64().unwrap();
      (format!(r#"{{"id":{id},"result":"allow"}}"#), vec![])
    });
    let mut local = vec![
      PathBuf::from("/dev/tty"),
      PathBuf::from("/dev/stdin"),
      PathBuf::from("/dev/stdout"),
      PathBuf::from("/dev/stderr"),
      PathBuf::from("/dev/fd/0"),
      PathBuf::from("/dev/fd/../fd/3"),
      dev_link.join("tty"),
    ];
    if cfg!(target_os = "linux") {
      local.extend([
        PathBuf::from("/proc/self/status"),
        PathBuf::from("/proc/thread-self/stat"),
        PathBuf::from("/proc/self/fd/1"),
        PathBuf::from(format!("/proc/{}/cmdline", std::process::id())),
      ]);
    }
    for path in &local {
      for options in [
        read_only(),
        BrokerOpenOptions {
          write: true,
          truncate: true,
          ..Default::default()
        },
      ] {
        assert!(
          matches!(
            broker.open(path, &options).unwrap(),
            Ok(BrokerOpenOutcome::OpenItself)
          ),
          "{}",
          path.display()
        );
      }
    }
    assert!(sent.lock().is_empty(), "{:?}", sent.lock());
    // Ordinary files (and other device nodes) still go to the broker.
    for path in [file.as_path(), Path::new("/dev/null")] {
      broker.open(path, &read_only()).unwrap().unwrap();
    }
    assert_eq!(sent.lock().len(), 2, "{:?}", sent.lock());
  }

  #[test]
  fn v2_refuses_the_wrong_access_mode() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data.txt");
    std::fs::write(&file, "public").unwrap();
    let file2 = file.clone();
    let broker = v2_broker(move |req| {
      let id = req["id"].as_u64().unwrap();
      // Asked for write, hands back a read-only descriptor... and the other
      // way around.
      let f = if req["open"]["write"] == true {
        open_nofollow(&file2)
      } else {
        std::fs::OpenOptions::new()
          .read(true)
          .write(true)
          .open(&file2)
          .unwrap()
      };
      (
        format!(r#"{{"id":{id},"result":"allow","fd":true}}"#),
        vec![f],
      )
    });
    let write = BrokerOpenOptions {
      write: true,
      ..Default::default()
    };
    assert!(broker.open(&file, &write).unwrap().is_err());
    assert!(broker.open(&file, &read_only()).unwrap().is_err());
  }

  #[test]
  fn v2_refuses_a_directory_for_writing_and_a_socket() {
    let dir = tempfile::tempdir().unwrap();
    let dir_path = dir.path().to_path_buf();
    let (sock_a, _sock_b) = UnixStream::pair().unwrap();
    let sock_fd = sock_a.as_raw_fd();
    let broker = v2_broker(move |req| {
      let id = req["id"].as_u64().unwrap();
      let f = if req["open"]["create"] == true {
        std::fs::File::open(&dir_path).unwrap()
      } else {
        // SAFETY: dup of a socket we keep open
        unsafe {
          <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(libc::dup(
            sock_fd,
          ))
        }
      };
      (
        format!(r#"{{"id":{id},"result":"allow","fd":true}}"#),
        vec![f],
      )
    });
    let create = BrokerOpenOptions {
      read: true,
      create: true,
      ..Default::default()
    };
    assert!(broker.open(dir.path(), &create).unwrap().is_err());
    assert!(
      broker
        .open(&dir.path().join("x"), &read_only())
        .unwrap()
        .is_err()
    );
  }

  /// The path is swapped for a symlink to a forbidden file between the
  /// broker's decision and Deno's use. Under v2 the broker's descriptor is
  /// what Deno uses: it is refused (its file has left that path) or it is the
  /// file the broker checked; the forbidden file is never read.
  #[test]
  fn v2_descriptor_matches_after_decision() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data.txt");
    let secret = dir.path().join("secret.txt");
    std::fs::write(&file, "public").unwrap();
    std::fs::write(&secret, "secret").unwrap();
    let (file2, secret2) = (file.clone(), secret.clone());
    let broker = v2_broker(move |req| {
      let id = req["id"].as_u64().unwrap();
      // The broker decides on, and opens, the file at the path...
      let f = open_nofollow(&file2);
      // ...then the path is swapped before Deno gets the answer.
      std::fs::rename(&file2, file2.with_extension("old")).unwrap();
      std::os::unix::fs::symlink(&secret2, &file2).unwrap();
      (
        format!(r#"{{"id":{id},"result":"allow","fd":true}}"#),
        vec![f],
      )
    });
    match broker.open(&file, &read_only()).unwrap() {
      Err(err) => {
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}")
      }
      Ok(BrokerOpenOutcome::File(mut f)) => {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut f, &mut text).unwrap();
        assert_eq!(text, "public");
      }
      Ok(BrokerOpenOutcome::OpenItself) => panic!("no descriptor"),
    }
  }

  /// A swap before the broker's open: a broker that opens with O_NOFOLLOW
  /// refuses the link, and nothing of the forbidden file reaches Deno.
  #[test]
  fn v2_descriptor_matches_before_open() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data.txt");
    let secret = dir.path().join("secret.txt");
    std::fs::write(&secret, "secret").unwrap();
    std::os::unix::fs::symlink(&secret, &file).unwrap();
    let file2 = file.clone();
    let broker = v2_broker(move |req| {
      use std::os::unix::fs::OpenOptionsExt;
      let id = req["id"].as_u64().unwrap();
      match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&file2)
      {
        Ok(f) => (
          format!(r#"{{"id":{id},"result":"allow","fd":true}}"#),
          vec![f],
        ),
        Err(_) => (
          format!(r#"{{"id":{id},"result":"deny","reason":"a link"}}"#),
          vec![],
        ),
      }
    });
    assert!(broker.open(&file, &read_only()).unwrap().is_err());
  }

  #[test]
  fn malformed_frames_are_protocol_errors() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data.txt");
    std::fs::write(&file, "public").unwrap();

    // A descriptor in answer to a v1 check.
    let file2 = file.clone();
    let (broker, _rx) = fake_broker(move |req| {
      let id = req["id"].as_u64().unwrap();
      (
        format!(r#"{{"id":{id},"result":"allow"}}"#),
        vec![open_nofollow(&file2)],
      )
    });
    assert!(broker.check("read", None).is_err());

    // Two lines for one request.
    let (broker, _rx) = fake_broker(|req| {
      let id = req["id"].as_u64().unwrap();
      (
        format!(
          "{{\"id\":{id},\"result\":\"allow\"}}\n{{\"id\":{id},\"result\":\"allow\"}}"
        ),
        vec![],
      )
    });
    assert!(broker.check("read", None).is_err());

    // An oversized line.
    let (broker, _rx) = fake_broker(|req| {
      let id = req["id"].as_u64().unwrap();
      let reason = "x".repeat(MAX_RESPONSE_BYTES);
      (
        format!(r#"{{"id":{id},"result":"deny","reason":"{reason}"}}"#),
        vec![],
      )
    });
    let err = broker.check("read", None).unwrap_err();
    assert!(err.to_string().contains("too long"), "{err}");

    // Not JSON.
    let (broker, _rx) = fake_broker(|_| ("hello".to_string(), vec![]));
    assert!(broker.check("read", None).is_err());

    // "fd":true without a descriptor, and two descriptors.
    let broker = v2_broker(|req| {
      let id = req["id"].as_u64().unwrap();
      (
        format!(r#"{{"id":{id},"result":"allow","fd":true}}"#),
        vec![],
      )
    });
    assert!(broker.open(&file, &read_only()).is_err());
    let file3 = file.clone();
    let broker = v2_broker(move |req| {
      let id = req["id"].as_u64().unwrap();
      (
        format!(r#"{{"id":{id},"result":"allow","fd":true}}"#),
        vec![open_nofollow(&file3), open_nofollow(&file3)],
      )
    });
    assert!(broker.open(&file, &read_only()).is_err());

    // A descriptor without "fd":true.
    let file4 = file.clone();
    let broker = v2_broker(move |req| {
      let id = req["id"].as_u64().unwrap();
      (
        format!(r#"{{"id":{id},"result":"allow"}}"#),
        vec![open_nofollow(&file4)],
      )
    });
    assert!(broker.open(&file, &read_only()).is_err());
  }

  #[test]
  fn a_closed_connection_is_an_error() {
    let (ours, theirs) = UnixStream::pair().unwrap();
    drop(theirs);
    let broker = PermissionBroker::from_pipe(IpcPipe::from_unix_stream(ours));
    assert!(broker.check("read", None).is_err());
  }
}
