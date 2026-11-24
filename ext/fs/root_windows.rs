// Copyright 2018-2026 the Deno authors. MIT license.

//! The Windows half of `Deno.openRoot()` (see [`crate::root`]).
//!
//! Every component of a path is opened with `NtCreateFile` relative to the
//! handle of the component before it (the first relative to the root's own
//! handle), with `FILE_OPEN_REPARSE_POINT`: the kernel never follows a
//! symlink, junction or mount point on the way, and a component that is
//! one is refused. Nothing is looked up by a full path, so a directory
//! renamed or swapped for a junction after the root was opened leads
//! nowhere new. `..` was resolved lexically before (`relative_components`).
//!
//! Not supported here: `symlink` and `readLink` (a link in a root is never
//! followed on Windows, so there is nothing to make one for), and POSIX
//! modes (`mode` is ignored, as `Deno.mkdir` ignores it on Windows).

#![allow(
  clippy::disallowed_methods,
  reason = "the root's own directory, after its permission check"
)]

use std::borrow::Cow;
use std::ffi::OsString;
use std::ffi::c_void;
use std::io;
use std::mem::ManuallyDrop;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::os::windows::io::FromRawHandle;
use std::os::windows::io::OwnedHandle;
use std::os::windows::io::RawHandle;
use std::path::Path;
use std::path::PathBuf;

use deno_io::fs::FsError;
use deno_io::fs::FsResult;
use deno_io::fs::FsStat;
use deno_permissions::CheckedPathBuf;
use deno_permissions::OpenAccessKind;
use deno_permissions::PermissionsContainer;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandleEx;
use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
use windows_sys::Win32::Storage::FileSystem::SetFileInformationByHandle;

use crate::OpenOptions;
use crate::interface::FsDirEntry;

// NtCreateFile's dispositions and options (ntifs.h)
const FILE_OPEN: u32 = 1;
const FILE_CREATE: u32 = 2;
const FILE_OPEN_IF: u32 = 3;
const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x0000_0020;
const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const OBJ_CASE_INSENSITIVE: u32 = 0x0000_0040;

// access rights (winnt.h)
const DELETE: u32 = 0x0001_0000;
const SYNCHRONIZE: u32 = 0x0010_0000;
const FILE_LIST_DIRECTORY: u32 = 0x0001;
const FILE_WRITE_DATA: u32 = 0x0002;
const FILE_TRAVERSE: u32 = 0x0020;
const FILE_READ_ATTRIBUTES: u32 = 0x0080;
const FILE_GENERIC_READ: u32 = 0x0012_0089;
const FILE_GENERIC_WRITE: u32 = 0x0012_0116;

const FILE_SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;

// FILE_INFORMATION_CLASS (NtSetInformationFile): FileRenameInformation,
// which honours a RootDirectory (SetFileInformationByHandle's FileRenameInfo
// refuses one with ERROR_INVALID_PARAMETER)
const FILE_RENAME_INFORMATION_CLASS: i32 = 10;
// FILE_INFO_BY_HANDLE_CLASS
const FILE_DISPOSITION_INFO_CLASS: i32 = 4;
const FILE_ATTRIBUTE_TAG_INFO_CLASS: i32 = 9;
const FILE_FULL_DIRECTORY_INFO_CLASS: i32 = 14;
const FILE_FULL_DIRECTORY_RESTART_INFO_CLASS: i32 = 15;
const FILE_DISPOSITION_INFO_EX_CLASS: i32 = 21;

const FILE_DISPOSITION_FLAG_DELETE: u32 = 0x1;
const FILE_DISPOSITION_FLAG_POSIX_SEMANTICS: u32 = 0x2;
const FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE: u32 = 0x10;

const ERROR_NO_MORE_FILES: i32 = 18;
const ERROR_INVALID_PARAMETER: i32 = 87;
const ERROR_NOT_SUPPORTED: i32 = 50;
const FILE_NAME_NORMALIZED: u32 = 0;
const VOLUME_NAME_DOS: u32 = 0;

#[repr(C)]
struct UnicodeString {
  length: u16,
  maximum_length: u16,
  buffer: *mut u16,
}

#[repr(C)]
struct ObjectAttributes {
  length: u32,
  root_directory: HANDLE,
  object_name: *const UnicodeString,
  attributes: u32,
  security_descriptor: *const c_void,
  security_quality_of_service: *const c_void,
}

#[repr(C)]
struct IoStatusBlock {
  status: usize,
  information: usize,
}

#[repr(C)]
struct FileAttributeTagInfo {
  file_attributes: u32,
  reparse_tag: u32,
}

#[repr(C)]
struct FileDispositionInfoEx {
  flags: u32,
}

#[repr(C)]
struct FileDispositionInfo {
  delete_file: u8,
}

/// `FILE_RENAME_INFORMATION`: `ReplaceIfExists` (a BOOLEAN in a union with
/// the `Flags` ULONG), the destination directory, the new name.
#[repr(C)]
struct FileRenameInfo {
  replace_if_exists: u32,
  root_directory: HANDLE,
  file_name_length: u32,
  file_name: [u16; 1],
}

#[link(name = "ntdll")]
unsafe extern "system" {
  fn NtCreateFile(
    file_handle: *mut HANDLE,
    desired_access: u32,
    object_attributes: *const ObjectAttributes,
    io_status_block: *mut IoStatusBlock,
    allocation_size: *const i64,
    file_attributes: u32,
    share_access: u32,
    create_disposition: u32,
    create_options: u32,
    ea_buffer: *const c_void,
    ea_length: u32,
  ) -> i32;
  fn NtSetInformationFile(
    file_handle: HANDLE,
    io_status_block: *mut IoStatusBlock,
    file_information: *const c_void,
    length: u32,
    file_information_class: i32,
  ) -> i32;
  fn RtlNtStatusToDosError(status: i32) -> u32;
}

/// An NTSTATUS failure as the Win32 error it maps to.
fn nt_error(status: i32) -> io::Error {
  // SAFETY: a plain conversion
  let code = unsafe { RtlNtStatusToDosError(status) };
  io::Error::from_raw_os_error(code as i32)
}

/// A refusal of a reparse point on the way (the root's escape error).
fn reparse_refused(rel: &str, root: &Path) -> FsError {
  FsError::Io(io::Error::new(
    io::ErrorKind::PermissionDenied,
    crate::root::RootEscape(format!(
      "Path \"{rel}\" goes through a symlink or junction in the root \"{}\", which a root doesn't follow on Windows",
      root.display()
    )),
  ))
}

/// `name` (one component, or empty for the directory itself) opened
/// relative to `dir`, never following a reparse point.
fn nt_open(
  dir: RawHandle,
  name: &[u16],
  access: u32,
  disposition: u32,
  options: u32,
) -> io::Result<OwnedHandle> {
  if name.len() * 2 > u16::MAX as usize {
    return Err(io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER));
  }
  let unicode = UnicodeString {
    length: (name.len() * 2) as u16,
    maximum_length: (name.len() * 2) as u16,
    buffer: name.as_ptr() as *mut u16,
  };
  let attributes = ObjectAttributes {
    length: std::mem::size_of::<ObjectAttributes>() as u32,
    root_directory: dir as HANDLE,
    object_name: &unicode,
    attributes: OBJ_CASE_INSENSITIVE,
    security_descriptor: std::ptr::null(),
    security_quality_of_service: std::ptr::null(),
  };
  let mut io_status = IoStatusBlock {
    status: 0,
    information: 0,
  };
  let mut handle: HANDLE = std::ptr::null_mut();
  // SAFETY: every pointer is valid for the call; `name` outlives it
  let status = unsafe {
    NtCreateFile(
      &mut handle,
      access,
      &attributes,
      &mut io_status,
      std::ptr::null(),
      FILE_ATTRIBUTE_NORMAL,
      FILE_SHARE_ALL,
      disposition,
      options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
      std::ptr::null(),
      0,
    )
  };
  if status < 0 {
    return Err(nt_error(status));
  }
  // SAFETY: a handle NtCreateFile just opened, owned by nobody else
  Ok(unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) })
}

/// The attributes and reparse tag of an open handle.
fn attribute_tag(handle: RawHandle) -> io::Result<(u32, u32)> {
  let mut info = FileAttributeTagInfo {
    file_attributes: 0,
    reparse_tag: 0,
  };
  // SAFETY: a valid handle and a buffer of the class's size
  let ok = unsafe {
    GetFileInformationByHandleEx(
      handle as HANDLE,
      FILE_ATTRIBUTE_TAG_INFO_CLASS,
      &mut info as *mut _ as *mut c_void,
      std::mem::size_of::<FileAttributeTagInfo>() as u32,
    )
  };
  if ok == 0 {
    return Err(io::Error::last_os_error());
  }
  Ok((info.file_attributes, info.reparse_tag))
}

fn is_reparse(handle: RawHandle) -> io::Result<bool> {
  Ok(attribute_tag(handle)?.0 & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

fn wide(name: &OsString) -> Vec<u16> {
  name.encode_wide().collect()
}

/// A handle borrowed as a `std::fs::File`, never closed by it.
fn as_file(handle: RawHandle) -> ManuallyDrop<std::fs::File> {
  // SAFETY: the handle stays open while the returned value lives; it is
  // never dropped
  ManuallyDrop::new(unsafe { std::fs::File::from_raw_handle(handle) })
}

/// Marks an open handle's file for deletion (POSIX semantics where the file
/// system has them: the name goes at once; read-only files too).
fn delete_on_close(handle: RawHandle) -> io::Result<()> {
  let info = FileDispositionInfoEx {
    flags: FILE_DISPOSITION_FLAG_DELETE
      | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
      | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
  };
  // SAFETY: a valid handle and a buffer of the class's size
  let ok = unsafe {
    SetFileInformationByHandle(
      handle as HANDLE,
      FILE_DISPOSITION_INFO_EX_CLASS,
      &info as *const _ as *const c_void,
      std::mem::size_of::<FileDispositionInfoEx>() as u32,
    )
  };
  if ok != 0 {
    return Ok(());
  }
  let err = io::Error::last_os_error();
  // FAT and older systems: the plain class
  if !matches!(
    err.raw_os_error(),
    Some(ERROR_INVALID_PARAMETER | ERROR_NOT_SUPPORTED)
  ) {
    return Err(err);
  }
  let info = FileDispositionInfo { delete_file: 1 };
  // SAFETY: as above
  let ok = unsafe {
    SetFileInformationByHandle(
      handle as HANDLE,
      FILE_DISPOSITION_INFO_CLASS,
      &info as *const _ as *const c_void,
      std::mem::size_of::<FileDispositionInfo>() as u32,
    )
  };
  if ok == 0 {
    return Err(io::Error::last_os_error());
  }
  Ok(())
}

/// One entry of a directory listing.
struct Entry {
  name: Vec<u16>,
  attributes: u32,
  reparse_tag: u32,
}

/// The entries of the directory `handle` (opened with
/// `FILE_LIST_DIRECTORY`), without `.` and `..`.
fn list(handle: RawHandle) -> io::Result<Vec<Entry>> {
  // FILE_FULL_DIR_INFO: NextEntryOffset @0, FileAttributes @56,
  // FileNameLength @60, EaSize (the reparse tag of a reparse point) @64,
  // FileName @68
  let mut buf = vec![0u64; 8192];
  let mut out = Vec::new();
  let mut class = FILE_FULL_DIRECTORY_RESTART_INFO_CLASS;
  loop {
    // SAFETY: a valid handle and a buffer of the given size
    let ok = unsafe {
      GetFileInformationByHandleEx(
        handle as HANDLE,
        class,
        buf.as_mut_ptr() as *mut c_void,
        (buf.len() * 8) as u32,
      )
    };
    if ok == 0 {
      let err = io::Error::last_os_error();
      if err.raw_os_error() == Some(ERROR_NO_MORE_FILES) {
        return Ok(out);
      }
      return Err(err);
    }
    class = FILE_FULL_DIRECTORY_INFO_CLASS;
    let bytes = buf.as_ptr() as *const u8;
    let total = buf.len() * 8;
    let mut offset = 0usize;
    loop {
      if offset + 68 > total {
        break;
      }
      // SAFETY: within the buffer the call filled, at the documented
      // offsets of FILE_FULL_DIR_INFO
      let (next, attributes, name_len, ea) = unsafe {
        let at = bytes.add(offset);
        (
          (at as *const u32).read_unaligned(),
          (at.add(56) as *const u32).read_unaligned(),
          (at.add(60) as *const u32).read_unaligned() as usize,
          (at.add(64) as *const u32).read_unaligned(),
        )
      };
      if offset + 68 + name_len > total {
        break;
      }
      let mut name = vec![0u16; name_len / 2];
      // SAFETY: `name_len` bytes of the name follow at offset 68
      unsafe {
        std::ptr::copy_nonoverlapping(
          bytes.add(offset + 68),
          name.as_mut_ptr() as *mut u8,
          name_len,
        );
      }
      let dots = name == [b'.' as u16] || name == [b'.' as u16, b'.' as u16];
      if !dots {
        out.push(Entry {
          name,
          attributes,
          reparse_tag: if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            ea
          } else {
            0
          },
        });
      }
      if next == 0 {
        break;
      }
      offset += next as usize;
    }
  }
}

/// Removes everything in the directory `dir`, entering no reparse point
/// (one is removed as itself).
fn empty_dir(dir: RawHandle) -> io::Result<()> {
  for entry in list(dir)? {
    let is_dir = entry.attributes & FILE_ATTRIBUTE_DIRECTORY != 0
      && entry.attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0;
    let access = if is_dir {
      DELETE | FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES
    } else {
      DELETE | FILE_READ_ATTRIBUTES
    };
    let child = nt_open(
      dir,
      &entry.name,
      access | SYNCHRONIZE,
      FILE_OPEN,
      if is_dir { FILE_DIRECTORY_FILE } else { 0 },
    )?;
    if is_dir && !is_reparse(child.as_raw_handle())? {
      empty_dir(child.as_raw_handle())?;
    }
    delete_on_close(child.as_raw_handle())?;
  }
  Ok(())
}

pub struct Root {
  /// The directory's path when it was opened.
  path: PathBuf,
  handle: OwnedHandle,
}

/// One operation's path: its components beneath the root (the permission
/// check was made on the path they spell).
pub struct Prepared {
  components: Vec<OsString>,
  abs: PathBuf,
  rel: String,
}

impl Prepared {
  pub fn display_path(&self) -> PathBuf {
    self.abs.clone()
  }
}

/// `\\?\C:\x` → `C:\x`, `\\?\UNC\server\share` → `\\server\share`.
fn strip_verbatim(path: Vec<u16>) -> PathBuf {
  let s = OsString::from_wide(&path);
  let text = s.to_string_lossy();
  if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
    return PathBuf::from(format!(r"\\{rest}"));
  }
  if let Some(rest) = text.strip_prefix(r"\\?\")
    && rest.as_bytes().get(1) == Some(&b':')
  {
    return PathBuf::from(rest);
  }
  PathBuf::from(s)
}

impl Root {
  /// `_requested` (the path as the caller gave it) is the Unix root's
  /// alias for absolute links inside it; no link is followed here.
  pub fn open_dir(path: &CheckedPathBuf, _requested: &Path) -> FsResult<Self> {
    let checked = path.as_checked_path();
    let file = std::fs::OpenOptions::new()
      .access_mode(
        FILE_LIST_DIRECTORY
          | FILE_TRAVERSE
          | FILE_READ_ATTRIBUTES
          | SYNCHRONIZE,
      )
      .share_mode(FILE_SHARE_ALL)
      .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
      .open(&*checked)?;
    if !file.metadata()?.is_dir() {
      return Err(FsError::Io(io::Error::new(
        io::ErrorKind::NotADirectory,
        "Not a directory",
      )));
    }
    let handle = OwnedHandle::from(file);
    let root = Self {
      path: PathBuf::new(),
      handle,
    };
    let path = root.final_path().unwrap_or_else(|| checked.to_path_buf());
    Ok(Self { path, ..root })
  }

  fn final_path(&self) -> Option<PathBuf> {
    let mut buf = vec![0u16; 1024];
    loop {
      // SAFETY: a valid handle and a buffer of the given length
      let len = unsafe {
        GetFinalPathNameByHandleW(
          self.handle.as_raw_handle() as HANDLE,
          buf.as_mut_ptr(),
          buf.len() as u32,
          FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
      } as usize;
      if len == 0 {
        return None;
      }
      if len < buf.len() {
        buf.truncate(len);
        return Some(strip_verbatim(buf));
      }
      buf.resize(len + 1, 0);
    }
  }

  pub fn current_path(&self) -> Option<PathBuf> {
    self.final_path()
  }

  /// Checks the operation's path as `Deno.open()` would: no link inside a
  /// root is followed here, so the path the components spell from where
  /// the directory is now is the object the operation reaches.
  pub fn prepare(
    &self,
    permissions: &PermissionsContainer,
    components: Vec<OsString>,
    access: OpenAccessKind,
    api_name: &str,
  ) -> FsResult<Prepared> {
    let base = self.final_path().unwrap_or_else(|| self.path.clone());
    let mut abs = base;
    let mut rel = PathBuf::new();
    for c in &components {
      abs.push(c);
      rel.push(c);
    }
    permissions
      .check_open(Cow::Borrowed(abs.as_path()), access, Some(api_name))
      .map_err(FsError::PermissionCheck)?;
    Ok(Prepared {
      components,
      abs,
      rel: rel.to_string_lossy().into_owned(),
    })
  }

  /// The directory the components before the last one name (`None`: the
  /// root itself), each opened beneath the one before, none a reparse
  /// point.
  fn walk(
    &self,
    p: &Prepared,
    dirs: &[OsString],
  ) -> FsResult<Option<OwnedHandle>> {
    let mut current: Option<OwnedHandle> = None;
    for name in dirs {
      let dir = current
        .as_ref()
        .map(|h| h.as_raw_handle())
        .unwrap_or_else(|| self.handle.as_raw_handle());
      let next = nt_open(
        dir,
        &wide(name),
        FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_OPEN,
        FILE_DIRECTORY_FILE,
      )?;
      if is_reparse(next.as_raw_handle())? {
        return Err(reparse_refused(&p.rel, &self.path));
      }
      current = Some(next);
    }
    Ok(current)
  }

  fn raw(&self, dir: &Option<OwnedHandle>) -> RawHandle {
    dir
      .as_ref()
      .map(|h| h.as_raw_handle())
      .unwrap_or_else(|| self.handle.as_raw_handle())
  }

  /// The object `p` names, opened with `access` (the root itself for an
  /// empty path); a reparse point is refused unless `allow_reparse`.
  fn open_final(
    &self,
    p: &Prepared,
    access: u32,
    disposition: u32,
    options: u32,
    allow_reparse: bool,
  ) -> FsResult<OwnedHandle> {
    let (dirs, name) = match p.components.split_last() {
      Some((last, dirs)) => (dirs, wide(last)),
      None => (&[][..], Vec::new()),
    };
    let parent = self.walk(p, dirs)?;
    let handle = nt_open(
      self.raw(&parent),
      &name,
      access | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
      disposition,
      options,
    )?;
    if !allow_reparse && is_reparse(handle.as_raw_handle())? {
      return Err(reparse_refused(&p.rel, &self.path));
    }
    Ok(handle)
  }

  pub fn open(
    &self,
    p: &Prepared,
    options: &OpenOptions,
  ) -> FsResult<std::fs::File> {
    let mut access = 0;
    if options.read || !(options.write || options.append) {
      access |= FILE_GENERIC_READ;
    }
    // append wins over write, as std's `OpenOptions` has it: without
    // `FILE_WRITE_DATA` every write lands at the end of the file
    if options.append {
      access |= FILE_GENERIC_WRITE & !FILE_WRITE_DATA;
    } else if options.write {
      access |= FILE_GENERIC_WRITE;
    }
    let disposition = if options.create_new {
      FILE_CREATE
    } else if options.create {
      FILE_OPEN_IF
    } else {
      FILE_OPEN
    };
    // never truncating in the open itself: a reparse point opened as
    // itself would lose its data before it is refused
    let handle = self.open_final(p, access, disposition, 0, false)?;
    let file = std::fs::File::from(handle);
    if options.truncate
      && !options.create_new
      && options.write
      && !options.append
    {
      file.set_len(0)?;
    }
    Ok(file)
  }

  pub fn stat(&self, p: &Prepared, follow: bool) -> FsResult<FsStat> {
    let handle = self.open_final(p, 0, FILE_OPEN, 0, !follow)?;
    let file = as_file(handle.as_raw_handle());
    Ok(FsStat::from_std(file.metadata()?))
  }

  fn mkdir_one(
    &self,
    parent: RawHandle,
    name: &OsString,
    p: &Prepared,
    disposition: u32,
  ) -> FsResult<OwnedHandle> {
    let handle = nt_open(
      parent,
      &wide(name),
      FILE_TRAVERSE | FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
      disposition,
      FILE_DIRECTORY_FILE,
    )?;
    if is_reparse(handle.as_raw_handle())? {
      return Err(reparse_refused(&p.rel, &self.path));
    }
    Ok(handle)
  }

  pub fn mkdir(
    &self,
    p: &Prepared,
    recursive: bool,
    _mode: u32,
  ) -> FsResult<()> {
    let Some((last, dirs)) = p.components.split_last() else {
      // the root itself exists
      return if recursive {
        Ok(())
      } else {
        Err(FsError::Io(io::Error::from(io::ErrorKind::AlreadyExists)))
      };
    };
    if !recursive {
      let parent = self.walk(p, dirs)?;
      self.mkdir_one(self.raw(&parent), last, p, FILE_CREATE)?;
      return Ok(());
    }
    let mut current: Option<OwnedHandle> = None;
    for name in &p.components {
      let next = self.mkdir_one(self.raw(&current), name, p, FILE_OPEN_IF)?;
      current = Some(next);
    }
    Ok(())
  }

  pub fn remove(&self, p: &Prepared, recursive: bool) -> FsResult<()> {
    if p.components.is_empty() {
      return Err(crate::root::outside_error(&p.rel, &self.path));
    }
    let handle = self.open_final(
      p,
      DELETE | FILE_LIST_DIRECTORY | FILE_TRAVERSE,
      FILE_OPEN,
      0,
      true,
    )?;
    let (attributes, _) = attribute_tag(handle.as_raw_handle())?;
    let is_dir = attributes & FILE_ATTRIBUTE_DIRECTORY != 0
      && attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0;
    if is_dir && recursive {
      empty_dir(handle.as_raw_handle())?;
    }
    delete_on_close(handle.as_raw_handle())?;
    Ok(())
  }

  pub fn rename(&self, from: &Prepared, to: &Prepared) -> FsResult<()> {
    let Some((to_name, to_dirs)) = to.components.split_last() else {
      return Err(crate::root::outside_error(&to.rel, &self.path));
    };
    if from.components.is_empty() {
      return Err(crate::root::outside_error(&from.rel, &self.path));
    }
    let source = self.open_final(from, DELETE, FILE_OPEN, 0, true)?;
    let to_parent = self.walk(to, to_dirs)?;
    let name = wide(to_name);
    let head = std::mem::offset_of!(FileRenameInfo, file_name);
    let size = head + name.len() * 2 + 2;
    let mut buf = vec![0u64; size.div_ceil(8)];
    let info = buf.as_mut_ptr() as *mut FileRenameInfo;
    // SAFETY: `buf` is large enough for the header and the name, and
    // 8-byte aligned
    unsafe {
      (*info).replace_if_exists = 1;
      (*info).root_directory = self.raw(&to_parent) as HANDLE;
      (*info).file_name_length = (name.len() * 2) as u32;
      std::ptr::copy_nonoverlapping(
        name.as_ptr(),
        (buf.as_mut_ptr() as *mut u8).add(head) as *mut u16,
        name.len(),
      );
    }
    let mut io_status = IoStatusBlock {
      status: 0,
      information: 0,
    };
    // SAFETY: a valid handle and the buffer built above
    let status = unsafe {
      NtSetInformationFile(
        source.as_raw_handle() as HANDLE,
        &mut io_status,
        buf.as_ptr() as *const c_void,
        size as u32,
        FILE_RENAME_INFORMATION_CLASS,
      )
    };
    if status < 0 {
      return Err(FsError::Io(nt_error(status)));
    }
    Ok(())
  }

  pub fn read_dir(&self, p: &Prepared) -> FsResult<Vec<FsDirEntry>> {
    let handle = self.open_final(
      p,
      FILE_LIST_DIRECTORY,
      FILE_OPEN,
      FILE_DIRECTORY_FILE,
      false,
    )?;
    Ok(
      list(handle.as_raw_handle())?
        .into_iter()
        .map(|entry| {
          let is_symlink = entry.reparse_tag == IO_REPARSE_TAG_SYMLINK
            || entry.reparse_tag == IO_REPARSE_TAG_MOUNT_POINT;
          let is_directory =
            !is_symlink && entry.attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
          FsDirEntry {
            name: OsString::from_wide(&entry.name)
              .to_string_lossy()
              .into_owned(),
            is_file: !is_symlink && !is_directory,
            is_directory,
            is_symlink,
          }
        })
        .collect(),
    )
  }

  pub fn symlink(&self, _target: &Path, _p: &Prepared) -> FsResult<()> {
    Err(FsError::NotSupported)
  }

  pub fn read_link(&self, _p: &Prepared) -> FsResult<PathBuf> {
    Err(FsError::NotSupported)
  }
}

#[cfg(test)]
#[allow(
  clippy::disallowed_types,
  reason = "the permissions container's parser"
)]
mod tests {
  use std::sync::Arc;

  use deno_permissions::RuntimePermissionDescriptorParser;

  use super::*;

  fn permissions() -> PermissionsContainer {
    PermissionsContainer::allow_all(Arc::new(
      RuntimePermissionDescriptorParser::new(sys_traits::impls::RealSys),
    ))
  }

  struct Bed {
    _tmp: tempfile::TempDir,
    root_dir: PathBuf,
    outside: PathBuf,
    root: Root,
    perms: PermissionsContainer,
  }

  fn bed() -> Bed {
    let tmp = tempfile::tempdir().unwrap();
    let root_dir = tmp.path().join("root");
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&root_dir).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "outside").unwrap();
    let root =
      Root::open_dir(&CheckedPathBuf::unsafe_new(root_dir.clone()), &root_dir)
        .unwrap();
    Bed {
      _tmp: tmp,
      root_dir,
      outside,
      root,
      perms: permissions(),
    }
  }

  impl Bed {
    fn p(&self, rel: &str, access: OpenAccessKind) -> Prepared {
      let comps = crate::root::relative_components(rel).unwrap();
      self
        .root
        .prepare(&self.perms, comps, access, "test")
        .unwrap()
    }

    fn junction(&self, target: &Path, rel: &str) {
      junction::create(target, self.root_dir.join(rel)).unwrap();
    }
  }

  fn is_escape(err: &FsError) -> bool {
    matches!(err, FsError::Io(e) if e.get_ref().is_some_and(|e| e.is::<crate::root::RootEscape>()))
  }

  fn read(b: &Bed, rel: &str) -> FsResult<String> {
    use std::io::Read;
    let mut f = b
      .root
      .open(&b.p(rel, OpenAccessKind::Read), &OpenOptions::read())?;
    let mut s = String::new();
    f.read_to_string(&mut s)?;
    Ok(s)
  }

  fn write(b: &Bed, rel: &str, data: &str) -> FsResult<()> {
    use std::io::Write;
    let opts = OpenOptions {
      read: false,
      write: true,
      create: true,
      truncate: true,
      append: false,
      create_new: false,
      custom_flags: None,
      mode: None,
    };
    let mut f = b.root.open(&b.p(rel, OpenAccessKind::Write), &opts)?;
    f.write_all(data.as_bytes())?;
    Ok(())
  }

  /// `Deno.Root.writeFile(…, { append: true })` passes write and append
  /// together: the bytes go to the end, never over the start.
  #[test]
  fn append_with_write_appends() {
    use std::io::Write;
    let b = bed();
    write(&b, "a.txt", "hello").unwrap();
    let opts = OpenOptions {
      read: false,
      write: true,
      create: true,
      truncate: false,
      append: true,
      create_new: false,
      custom_flags: None,
      mode: None,
    };
    let mut f = b
      .root
      .open(&b.p("a.txt", OpenAccessKind::Write), &opts)
      .unwrap();
    f.write_all(b" world").unwrap();
    drop(f);
    assert_eq!(read(&b, "a.txt").unwrap(), "hello world");
  }

  #[test]
  fn files_and_folders_beneath_the_root() {
    let b = bed();
    write(&b, "a.txt", "hello").unwrap();
    assert_eq!(read(&b, "a.txt").unwrap(), "hello");
    write(&b, "a.txt", "hi").unwrap();
    assert_eq!(read(&b, "a.txt").unwrap(), "hi");
    assert!(
      b.root
        .open(&b.p("nope.txt", OpenAccessKind::Read), &OpenOptions::read())
        .is_err()
    );

    b.root
      .mkdir(&b.p("d/e/f", OpenAccessKind::Write), true, 0o777)
      .unwrap();
    b.root
      .mkdir(&b.p("d/e/f", OpenAccessKind::Write), true, 0o777)
      .unwrap();
    assert!(
      b.root
        .mkdir(&b.p("d", OpenAccessKind::Write), false, 0o777)
        .is_err()
    );
    write(&b, "d/e/f/g.txt", "deep").unwrap();
    assert_eq!(read(&b, "d/e/../e/f/g.txt").unwrap(), "deep");
    assert!(b.root_dir.join("d/e/f/g.txt").is_file());

    let st = b
      .root
      .stat(&b.p("d/e/f/g.txt", OpenAccessKind::Read), true)
      .unwrap();
    assert!(st.is_file);
    assert_eq!(st.size, 4);
    assert!(
      b.root
        .stat(&b.p("", OpenAccessKind::Read), true)
        .unwrap()
        .is_directory
    );

    let mut names: Vec<_> = b
      .root
      .read_dir(&b.p("", OpenAccessKind::Read))
      .unwrap()
      .into_iter()
      .map(|e| (e.name, e.is_directory))
      .collect();
    names.sort();
    assert_eq!(names, vec![("a.txt".into(), false), ("d".into(), true)]);

    b.root
      .rename(
        &b.p("a.txt", OpenAccessKind::Write),
        &b.p("d/e/moved.txt", OpenAccessKind::Write),
      )
      .unwrap();
    assert_eq!(read(&b, "d/e/moved.txt").unwrap(), "hi");
    assert!(!b.root_dir.join("a.txt").exists());

    // a read-only file goes too
    let ro = b.root_dir.join("d/e/f/g.txt");
    let mut perms = std::fs::metadata(&ro).unwrap().permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&ro, perms).unwrap();
    assert!(
      b.root
        .remove(&b.p("d", OpenAccessKind::Write), false)
        .is_err()
    );
    b.root
      .remove(&b.p("d", OpenAccessKind::Write), true)
      .unwrap();
    assert!(!b.root_dir.join("d").exists());
    // the root itself can't be removed or renamed
    assert!(is_escape(
      &b.root
        .remove(&b.p("", OpenAccessKind::Write), true)
        .unwrap_err()
    ));
  }

  #[test]
  fn a_junction_is_never_followed() {
    let b = bed();
    b.junction(&b.outside, "out");
    let err = read(&b, "out/secret.txt").unwrap_err();
    assert!(is_escape(&err), "{err:?}");
    let err = write(&b, "out/planted.txt", "x").unwrap_err();
    assert!(is_escape(&err), "{err:?}");
    assert!(!b.outside.join("planted.txt").exists());
    assert!(is_escape(
      &b.root
        .stat(&b.p("out", OpenAccessKind::Read), true)
        .err()
        .unwrap()
    ));
    assert!(
      b.root
        .stat(&b.p("out", OpenAccessKind::Read), false)
        .unwrap()
        .is_symlink
    );
    assert!(is_escape(
      &b.root
        .read_dir(&b.p("out", OpenAccessKind::Read))
        .unwrap_err()
    ));
    assert!(is_escape(
      &b.root
        .mkdir(&b.p("out/sub", OpenAccessKind::Write), true, 0o777)
        .unwrap_err()
    ));
    assert!(!b.outside.join("sub").exists());
    let listed = b.root.read_dir(&b.p("", OpenAccessKind::Read)).unwrap();
    assert!(listed.iter().any(|e| e.name == "out" && e.is_symlink));

    // a recursive remove takes the junction, not what it leads to
    b.root
      .mkdir(&b.p("holder", OpenAccessKind::Write), false, 0o777)
      .unwrap();
    b.junction(&b.outside, "holder/out");
    b.root
      .remove(&b.p("holder", OpenAccessKind::Write), true)
      .unwrap();
    assert_eq!(
      std::fs::read_to_string(b.outside.join("secret.txt")).unwrap(),
      "outside"
    );

    // a junction that stays inside isn't followed either
    b.root
      .mkdir(&b.p("real", OpenAccessKind::Write), false, 0o777)
      .unwrap();
    write(&b, "real/f.txt", "inside").unwrap();
    b.junction(&b.root_dir.join("real"), "alias");
    assert!(is_escape(&read(&b, "alias/f.txt").unwrap_err()));
  }

  #[test]
  fn the_root_holds_its_folder_when_the_path_is_swapped() {
    let b = bed();
    write(&b, "f.txt", "original").unwrap();
    let moved = b.root_dir.with_file_name("moved");
    std::fs::rename(&b.root_dir, &moved).unwrap();
    junction::create(&b.outside, &b.root_dir).unwrap();
    assert_eq!(read(&b, "f.txt").unwrap(), "original");
    assert!(read(&b, "secret.txt").is_err());
    write(&b, "g.txt", "written").unwrap();
    assert_eq!(
      std::fs::read_to_string(moved.join("g.txt")).unwrap(),
      "written"
    );
    assert!(!b.outside.join("g.txt").exists());
    assert_eq!(
      b.root.current_path().unwrap(),
      moved
        .canonicalize()
        .map(|p| strip_verbatim(p.as_os_str().encode_wide().collect()))
        .unwrap()
    );
  }
}
