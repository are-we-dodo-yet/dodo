// Copyright 2018-2026 the Deno authors. MIT license.

use std::ffi::OsStr;
use std::io::Read;
use std::io::Write;
use std::io::{self};

pub struct IpcPipe(Inner);

#[cfg(unix)]
type Inner = std::os::unix::net::UnixStream;

#[cfg(not(unix))]
type Inner = std::fs::File;

impl IpcPipe {
  /// Connect to a local IPC endpoint.
  /// - Unix: `addr` like `/tmp/deno.sock`
  /// - Windows: `addr` like `\\.\pipe\deno-permission-broker`
  pub fn connect(addr: impl AsRef<OsStr>) -> io::Result<Self> {
    Self::connect_impl(addr.as_ref())
  }
}

#[cfg(unix)]
impl IpcPipe {
  fn connect_impl(addr: &OsStr) -> io::Result<Self> {
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    let s = UnixStream::connect(Path::new(addr))?;
    s.set_nonblocking(false)?;
    Ok(Self(s))
  }
}

#[cfg(windows)]
impl IpcPipe {
  fn connect_impl(addr: &OsStr) -> io::Result<Self> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;

    use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::CreateFileW;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
    use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_READ;
    use windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_WRITE;
    use windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING;
    use windows_sys::Win32::System::Pipes::NMPWAIT_WAIT_FOREVER;
    use windows_sys::Win32::System::Pipes::PIPE_READMODE_BYTE;
    use windows_sys::Win32::System::Pipes::SetNamedPipeHandleState;
    use windows_sys::Win32::System::Pipes::WaitNamedPipeW;

    // OsStr -> UTF-16 + NUL
    let mut wide: Vec<u16> = addr.encode_wide().collect();
    wide.push(0);

    // Try to open; if the pipe is busy, wait and retry.
    let handle = loop {
      // SAFETY: WinAPI call
      let h = unsafe {
        CreateFileW(
          wide.as_ptr(),
          FILE_GENERIC_READ | FILE_GENERIC_WRITE,
          0, // no sharing
          std::ptr::null(),
          OPEN_EXISTING,
          FILE_ATTRIBUTE_NORMAL, // blocking
          std::ptr::null_mut(),
        )
      };
      if h != INVALID_HANDLE_VALUE {
        break h;
      }
      let err = io::Error::last_os_error();
      if err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) {
        // SAFETY: WinAPI call
        unsafe { WaitNamedPipeW(wide.as_ptr(), NMPWAIT_WAIT_FOREVER) };
        continue;
      } else {
        return Err(err);
      }
    };

    // Ensure byte mode to mirror Unix stream semantics.
    // SAFETY: WinAPI call
    unsafe {
      let _ = SetNamedPipeHandleState(
        handle,
        &PIPE_READMODE_BYTE,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
      );
    }

    // SAFETY: Passing WinAPI handle
    let file = unsafe { std::fs::File::from_raw_handle(handle as _) };
    Ok(Self(file))
  }
}

#[cfg(all(not(unix), not(windows)))]
impl IpcPipe {
  fn connect_impl(_addr: &OsStr) -> io::Result<Self> {
    Err(io::Error::new(
      io::ErrorKind::Unsupported,
      "Platform not supported.",
    ))
  }
}

fn too_long(max: usize) -> io::Error {
  io::Error::new(
    io::ErrorKind::InvalidData,
    format!(
      "Permission broker sent a malformed response: a line too long (over {max} bytes)"
    ),
  )
}

fn trailing_bytes() -> io::Error {
  io::Error::new(
    io::ErrorKind::InvalidData,
    "Permission broker sent a malformed response: bytes after the end of the response line",
  )
}

/// Appends `chunk` to `line`; returns true once the line is complete (its
/// `\n` is not kept). Bytes after the `\n`, or a line longer than `max`, are
/// errors: one request is outstanding at a time, so a broker never has more
/// than one line to send.
fn push_chunk(
  line: &mut Vec<u8>,
  chunk: &[u8],
  max: usize,
) -> io::Result<bool> {
  match chunk.iter().position(|b| *b == b'\n') {
    Some(pos) => {
      if pos + 1 != chunk.len() {
        return Err(trailing_bytes());
      }
      line.extend_from_slice(&chunk[..pos]);
      if line.len() > max {
        return Err(too_long(max));
      }
      Ok(true)
    }
    None => {
      line.extend_from_slice(chunk);
      if line.len() > max {
        return Err(too_long(max));
      }
      Ok(false)
    }
  }
}

#[cfg(unix)]
impl IpcPipe {
  #[cfg(test)]
  pub fn from_unix_stream(stream: std::os::unix::net::UnixStream) -> Self {
    Self(stream)
  }

  /// Reads one `\n`-terminated line with `recvmsg`, collecting any
  /// descriptors sent with it (`SCM_RIGHTS`). The descriptors are close-on-exec
  /// (`MSG_CMSG_CLOEXEC` where the kernel has it, else set right after the
  /// receive).
  ///
  /// Without `MSG_CMSG_CLOEXEC` (macOS) there is a window between `recvmsg`
  /// returning and the `F_SETFD`: a thread that forks and execs at that
  /// instant (`Deno.Command` on another worker) passes the descriptor to the
  /// child. macOS has no atomic alternative for `SCM_RIGHTS`. The descriptor
  /// is one the broker chose to give this process for a file it already
  /// allowed, so the leak is to a child of this process, not across a trust
  /// boundary: a program this process may start already runs with the user's
  /// rights, and with no run permission none is started.
  pub fn recv_line(
    &mut self,
    max: usize,
  ) -> io::Result<(Vec<u8>, Vec<std::os::fd::OwnedFd>)> {
    use std::os::fd::AsRawFd;
    use std::os::fd::FromRawFd;
    use std::os::fd::OwnedFd;

    let sock = self.0.as_raw_fd();
    let mut line = Vec::new();
    let mut fds: Vec<OwnedFd> = Vec::new();
    loop {
      let mut buf = [0u8; 4096];
      let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: buf.len(),
      };
      // Room for a handful of descriptors; more than fit is MSG_CTRUNC.
      let mut control = [0u64; 32];
      #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd"
      ))]
      let flags = libc::MSG_CMSG_CLOEXEC;
      #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd"
      )))]
      let flags = 0;
      // SAFETY: a msghdr over stack buffers; the control messages are read
      // through the CMSG macros within msg_controllen
      let (received, msg_flags) = unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
        msg.msg_controllen = std::mem::size_of_val(&control) as _;
        let received = libc::recvmsg(sock, &mut msg, flags);
        if received >= 0 {
          let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
          while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET
              && (*cmsg).cmsg_type == libc::SCM_RIGHTS
            {
              let data = libc::CMSG_DATA(cmsg) as *const libc::c_int;
              let header = libc::CMSG_LEN(0) as usize;
              let len = ((*cmsg).cmsg_len as usize).saturating_sub(header);
              for i in 0..len / std::mem::size_of::<libc::c_int>() {
                let fd = std::ptr::read_unaligned(data.add(i));
                if fd >= 0 {
                  let fd = OwnedFd::from_raw_fd(fd);
                  #[cfg(not(any(
                    target_os = "linux",
                    target_os = "android",
                    target_os = "freebsd"
                  )))]
                  libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
                  fds.push(fd);
                }
              }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
          }
        }
        (received, msg.msg_flags)
      };
      if received < 0 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::Interrupted {
          continue;
        }
        return Err(err);
      }
      if msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          "Permission broker sent a malformed response: too many descriptors",
        ));
      }
      if received == 0 {
        return Err(io::Error::new(
          io::ErrorKind::UnexpectedEof,
          "Permission broker closed the connection",
        ));
      }
      if push_chunk(&mut line, &buf[..received as usize], max)? {
        return Ok((line, fds));
      }
    }
  }
}

#[cfg(not(unix))]
impl IpcPipe {
  /// Reads one `\n`-terminated line (no descriptors on this platform).
  pub fn recv_line(&mut self, max: usize) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
      let mut buf = [0u8; 4096];
      let n = self.0.read(&mut buf)?;
      if n == 0 {
        return Err(io::Error::new(
          io::ErrorKind::UnexpectedEof,
          "Permission broker closed the connection",
        ));
      }
      if push_chunk(&mut line, &buf[..n], max)? {
        return Ok(line);
      }
    }
  }
}

impl Read for IpcPipe {
  fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
    self.0.read(buf)
  }
}

impl Write for IpcPipe {
  fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
    self.0.write(buf)
  }

  fn flush(&mut self) -> io::Result<()> {
    self.0.flush()
  }
}
