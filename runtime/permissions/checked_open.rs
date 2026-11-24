// Copyright 2018-2026 the Deno authors. MIT license.

//! Race-safe opens: a path check that hands back an open descriptor
//! instead of a yes / no, so the object the check looked at is the object
//! the operation uses.
//!
//! Resolving a path and then opening the string again leaves a window: a
//! symlink swapped in between (by the program, another process, or another
//! user who can write to a directory on the path) is followed without a new
//! check. Here the open IS the check:
//!
//! - [`open_checked`] opens a file or directory beneath one of the
//!   [`OpenScope`]'s allow roots and returns a [`CheckedFile`];
//! - [`parent_checked`] opens the directory that holds a path's last
//!   component and returns a [`CheckedParent`], whose name operations
//!   (`mkdir`, `unlink`, `rename`, `symlink`, ...) use the `*at` syscalls on
//!   that descriptor.
//!
//! How a path is opened:
//!
//! - Each allow root is opened once (by its path, or handed in already open
//!   with [`OpenScope::with_open_root`]) and the descriptor kept, so a root
//!   swapped later doesn't move the scope.
//! - Linux 5.6+: `openat2(root, rel, RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)`
//!   (plus `RESOLVE_NO_SYMLINKS` for [`Symlinks::Never`]), which refuses `..`
//!   and symlinks that leave the root atomically. It is used only when no
//!   deny lies beneath (or above) the root; when it refuses (`EXDEV`: an
//!   absolute symlink, which can still lead to another allowed place;
//!   `EAGAIN`, `ELOOP`) the component walk decides.
//! - Everywhere else (macOS, older kernels, a deny inside the root): a walk
//!   of the components with `openat(dir, component, O_NOFOLLOW |
//!   O_DIRECTORY)`. A symlink is read (`readlinkat`), its target re-checked
//!   against the roots and the denies as a whole new path, and the walk
//!   restarts from the root that holds it (at most 40 links).
//! - After the open, when the scope has denies, the descriptor's own path
//!   (`F_GETPATH` on macOS, `/proc/self/fd/N` on Linux) is checked against
//!   them: the descriptor pins the object, so a later rename doesn't matter.
//!   A path that can't be read back (no `/proc`, a deleted file) counts as
//!   denied. A file this call created (`create_new`) and then found denied is
//!   removed again.
//!
//! What this does not change: a hard link to a denied file placed in an
//! allowed directory is readable (path semantics, as in `check_open`);
//! `..` is resolved lexically before anything is opened, as the permission
//! checks resolve it; the roots and denies are compared as given
//! (case-insensitively on macOS for denies, which only refuses more), so a
//! caller passes the resolved form of a scope too where it differs (`/tmp`
//! and `/private/tmp`).
//!
//! Used by `Deno.openRoot()` (`ext/fs/root.rs`) on Unix.

use std::ffi::CString;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::io;
use std::os::fd::AsFd;
use std::os::fd::AsRawFd;
use std::os::fd::BorrowedFd;
use std::os::fd::FromRawFd;
use std::os::fd::IntoRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::ffi::OsStringExt;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;

/// The most symlinks one open follows (Linux's `MAXSYMLINKS`).
const MAX_HOPS: u32 = 40;

/// Where an open may reach: beneath the allow roots (or anywhere), outside
/// the denies.
pub struct OpenScope {
  /// `None`: unscoped (`--allow-read` with no list).
  roots: Option<Vec<Root>>,
  deny: Vec<PathBuf>,
  walk_only: bool,
}

struct Root {
  path: PathBuf,
  /// Opened on first use; a root that doesn't exist yet is tried again.
  fd: Mutex<Option<Arc<OwnedFd>>>,
}

impl OpenScope {
  /// `roots` and `deny` must be absolute; they are normalized lexically.
  /// `roots: None` allows every path outside the denies.
  pub fn new(
    roots: Option<Vec<PathBuf>>,
    deny: Vec<PathBuf>,
  ) -> io::Result<Self> {
    let absolute = |p: PathBuf| {
      normalize_absolute(&p).ok_or_else(|| {
        io::Error::new(
          io::ErrorKind::InvalidInput,
          format!("not an absolute path: {}", p.display()),
        )
      })
    };
    let roots = match roots {
      Some(roots) => Some(
        roots
          .into_iter()
          .map(|p| {
            Ok(Root {
              path: absolute(p)?,
              fd: Mutex::new(None),
            })
          })
          .collect::<io::Result<Vec<_>>>()?,
      ),
      None => None,
    };
    let deny = deny.into_iter().map(absolute).collect::<io::Result<_>>()?;
    Ok(Self {
      roots,
      deny,
      walk_only: false,
    })
  }

  /// A scope of one root whose directory is already open
  /// (`Deno.openRoot`): `fd` is the directory, `paths` the absolute paths it
  /// is known by (its location when it was opened, then other spellings of
  /// it, such as the path it was opened by before `/var` resolved to
  /// `/private/var`), each a root that opens through `fd`. The root is never
  /// opened by a path again, so a directory moved, or swapped for a link,
  /// after `fd` was opened doesn't move the scope: every open goes through
  /// `fd`.
  pub fn with_open_root(
    paths: Vec<PathBuf>,
    fd: Arc<OwnedFd>,
    deny: Vec<PathBuf>,
  ) -> io::Result<Self> {
    let mut scope = Self::new(Some(paths), deny)?;
    if let Some(roots) = &mut scope.roots {
      for root in roots {
        *root.fd.get_mut() = Some(fd.clone());
      }
    }
    Ok(scope)
  }

  /// Never uses `openat2`, only the component walk (tests and benchmarks).
  pub fn force_component_walk(mut self, walk_only: bool) -> Self {
    self.walk_only = walk_only;
    self
  }

  fn is_denied(&self, path: &Path) -> bool {
    self.deny.iter().any(|d| path_starts_with(path, d))
  }

  /// The root that holds `path` (the longest), with its descriptor.
  fn root_for(
    &self,
    path: &Path,
  ) -> Result<(PathBuf, Arc<OwnedFd>), CheckedOpenError> {
    let not_capable = || CheckedOpenError::NotCapable(path.to_path_buf());
    let Some(roots) = &self.roots else {
      let root = PathBuf::from("/");
      return Ok((root.clone(), open_root(&root).map_err(io_err(path))?));
    };
    let root = roots
      .iter()
      .filter(|r| path.starts_with(&r.path))
      .max_by_key(|r| r.path.as_os_str().len())
      .ok_or_else(not_capable)?;
    let mut fd = root.fd.lock();
    if let Some(fd) = &*fd {
      return Ok((root.path.clone(), fd.clone()));
    }
    let opened = match open_root(&root.path) {
      Ok(opened) => opened,
      // a root that is a file (`--allow-read=/etc/hosts`), or a link to
      // one, or a root that doesn't exist yet (`--allow-write=out.txt`,
      // `--allow-write=node_modules`, to be created): the scope is that one
      // name in its directory. The directory is opened by path, as a root
      // is, and not kept (a root created later is opened as one then);
      // `in_file_root` keeps the lookup to the walk, which never resolves
      // beneath it, so the name itself (and nothing else in its directory)
      // is reached, and a path beneath a root still missing isn't found.
      Err(err)
        if matches!(err.raw_os_error(), Some(libc::ENOTDIR | libc::ENOENT)) =>
      {
        let parent = root.path.parent().ok_or_else(not_capable)?;
        let dir = open_root(parent).map_err(io_err(path))?;
        return Ok((parent.to_path_buf(), dir));
      }
      Err(err) => return Err(io_err(path)(err)),
    };
    *fd = Some(opened.clone());
    Ok((root.path.clone(), opened))
  }

  /// Whether `root` (as [`Self::root_for`] returned it for `path`) is the
  /// directory of a file root rather than a root: nothing but the path
  /// itself is in scope beneath it.
  #[cfg(target_os = "linux")]
  fn in_file_root(&self, root: &Path, path: &Path) -> bool {
    match &self.roots {
      None => false,
      Some(roots) => !roots
        .iter()
        .any(|r| r.path == root && path.starts_with(&r.path)),
    }
  }

  /// Whether a deny lies beneath `root` or covers it (then `openat2`
  /// can't hold it and the walk does).
  #[cfg(target_os = "linux")]
  fn deny_touches(&self, root: &Path) -> bool {
    self
      .deny
      .iter()
      .any(|d| path_starts_with(d, root) || path_starts_with(root, d))
  }

  /// The check after an open: the descriptor's own path is outside the
  /// denies (or can't be read back, which counts as denied).
  fn post_open_check(&self, fd: BorrowedFd<'_>, name: Option<&OsStr>) -> bool {
    if self.deny.is_empty() {
      return true;
    }
    let Some(mut path) = fd_path(fd) else {
      return false;
    };
    if let Some(name) = name {
      path.push(name);
    }
    !self.is_denied(&path)
  }
}

/// What an open does with symlinks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Symlinks {
  /// Every link is followed (each one checked).
  Follow,
  /// Links among the directories are followed; a link as the last
  /// component is not (`O_NOFOLLOW`: `ELOOP`).
  NoFollowFinal,
  /// No link anywhere on the path (`ELOOP`).
  Never,
}

/// How [`open_checked`] opens the file.
#[derive(Debug, Clone)]
pub struct CheckedOpenOptions {
  read: bool,
  write: bool,
  append: bool,
  truncate: bool,
  create: bool,
  create_new: bool,
  directory: bool,
  nonblocking: bool,
  path_only: bool,
  custom_flags: libc::c_int,
  mode: u32,
  symlinks: Symlinks,
}

impl Default for CheckedOpenOptions {
  fn default() -> Self {
    Self::new()
  }
}

impl CheckedOpenOptions {
  /// Nothing set: add `read` / `write` / ...; symlinks are followed.
  pub fn new() -> Self {
    Self {
      read: false,
      write: false,
      append: false,
      truncate: false,
      create: false,
      create_new: false,
      directory: false,
      nonblocking: false,
      path_only: false,
      custom_flags: 0,
      mode: 0o666,
      symlinks: Symlinks::Follow,
    }
  }

  pub fn read(mut self, v: bool) -> Self {
    self.read = v;
    self
  }
  pub fn write(mut self, v: bool) -> Self {
    self.write = v;
    self
  }
  pub fn append(mut self, v: bool) -> Self {
    self.append = v;
    self
  }
  pub fn truncate(mut self, v: bool) -> Self {
    self.truncate = v;
    self
  }
  pub fn create(mut self, v: bool) -> Self {
    self.create = v;
    self
  }
  /// `O_CREAT | O_EXCL`: never opens what is already there, link or not.
  pub fn create_new(mut self, v: bool) -> Self {
    self.create_new = v;
    self
  }
  /// Opens a directory (for [`CheckedFile::read_dir`]).
  pub fn directory(mut self, v: bool) -> Self {
    self.directory = v;
    self
  }
  /// `O_NONBLOCK`: the open itself never waits (a FIFO with no peer, a
  /// device).
  pub fn nonblocking(mut self, v: bool) -> Self {
    self.nonblocking = v;
    self
  }
  /// Linux `O_PATH`: a descriptor that names the object without opening it
  /// (no read or search right on it is needed; it reads and writes
  /// nothing). Ignored elsewhere.
  pub fn path_only(mut self, v: bool) -> Self {
    self.path_only = v;
    self
  }
  /// The caller's own `open(2)` flags, added to the ones the options make
  /// (`O_NOFOLLOW` here asks for [`Symlinks::NoFollowFinal`]; the access
  /// mode bits and `O_PATH` / `O_TMPFILE` are dropped: the options decide
  /// those).
  pub fn custom_flags(mut self, flags: libc::c_int) -> Self {
    #[cfg(target_os = "linux")]
    let dropped =
      libc::O_ACCMODE | libc::O_NOFOLLOW | libc::O_PATH | libc::O_TMPFILE;
    #[cfg(not(target_os = "linux"))]
    let dropped = libc::O_ACCMODE | libc::O_NOFOLLOW;
    if flags & libc::O_NOFOLLOW != 0 && self.symlinks == Symlinks::Follow {
      self.symlinks = Symlinks::NoFollowFinal;
    }
    // O_TMPFILE includes O_DIRECTORY's bit on Linux: keep a plain
    // O_DIRECTORY the caller asked for
    let directory = flags & libc::O_DIRECTORY != 0;
    #[cfg(target_os = "linux")]
    let directory = directory && flags & libc::O_TMPFILE != libc::O_TMPFILE;
    self.custom_flags = flags & !dropped;
    if directory {
      self.custom_flags |= libc::O_DIRECTORY;
    }
    self
  }
  pub fn mode(mut self, mode: u32) -> Self {
    self.mode = mode;
    self
  }
  pub fn symlinks(mut self, symlinks: Symlinks) -> Self {
    self.symlinks = symlinks;
    self
  }

  fn flags(&self) -> libc::c_int {
    let mut flags = match (self.read, self.write || self.append) {
      (_, false) => libc::O_RDONLY,
      (false, true) => libc::O_WRONLY,
      (true, true) => libc::O_RDWR,
    };
    if self.append {
      flags |= libc::O_APPEND;
    }
    if self.truncate {
      flags |= libc::O_TRUNC;
    }
    if self.create_new {
      flags |= libc::O_CREAT | libc::O_EXCL;
    } else if self.create {
      flags |= libc::O_CREAT;
    }
    if self.directory {
      flags |= libc::O_DIRECTORY;
    }
    if self.nonblocking {
      flags |= libc::O_NONBLOCK;
    }
    #[cfg(target_os = "linux")]
    if self.path_only {
      // O_PATH takes only O_DIRECTORY, O_NOFOLLOW and O_CLOEXEC
      flags = libc::O_PATH | (flags & libc::O_DIRECTORY);
      return flags | libc::O_CLOEXEC;
    }
    flags | self.custom_flags | libc::O_CLOEXEC
  }
}

/// An open file or directory that passed the check.
#[derive(Debug)]
pub struct CheckedFile {
  fd: OwnedFd,
  display: PathBuf,
}

impl CheckedFile {
  /// The path as the caller gave it (for messages).
  pub fn path(&self) -> &Path {
    &self.display
  }

  pub fn into_fd(self) -> OwnedFd {
    self.fd
  }

  pub fn into_std_file(self) -> std::fs::File {
    std::fs::File::from(self.fd)
  }

  /// The names in an opened directory (without `.` and `..`).
  pub fn read_dir(&self) -> io::Result<Vec<OsString>> {
    read_dir_fd(self.fd.as_fd())
  }

  /// Where the opened object is now, as the kernel names it (`None` when
  /// it can't be told: no `/proc`, an unlinked file).
  pub fn real_path(&self) -> Option<PathBuf> {
    fd_path(self.fd.as_fd())
  }
}

impl AsFd for CheckedFile {
  fn as_fd(&self) -> BorrowedFd<'_> {
    self.fd.as_fd()
  }
}

/// The directory that holds a checked path's last component, open, and the
/// component's name: name operations act on `(dir, name)` with the `*at`
/// syscalls and never resolve the path again.
#[derive(Debug)]
pub struct CheckedParent {
  dir: OwnedFd,
  name: OsString,
  display: PathBuf,
}

impl CheckedParent {
  pub fn path(&self) -> &Path {
    &self.display
  }

  pub fn name(&self) -> &OsStr {
    &self.name
  }

  pub fn dir(&self) -> BorrowedFd<'_> {
    self.dir.as_fd()
  }

  pub fn mkdir(&self, mode: u32) -> io::Result<()> {
    let name = cstr(&self.name)?;
    // SAFETY: a valid descriptor and C string
    cvt(unsafe {
      libc::mkdirat(self.dir.as_raw_fd(), name.as_ptr(), mode as libc::mode_t)
    })
  }

  /// Removes a file or a symlink (the link itself).
  pub fn remove_file(&self) -> io::Result<()> {
    self.unlinkat(0)
  }

  pub fn remove_dir(&self) -> io::Result<()> {
    self.unlinkat(libc::AT_REMOVEDIR)
  }

  fn unlinkat(&self, flags: libc::c_int) -> io::Result<()> {
    let name = cstr(&self.name)?;
    // SAFETY: a valid descriptor and C string
    cvt(unsafe { libc::unlinkat(self.dir.as_raw_fd(), name.as_ptr(), flags) })
  }

  /// Where the entry is now: the directory as the kernel names it, with
  /// the name (`None` when the directory's path can't be told).
  pub fn real_path(&self) -> Option<PathBuf> {
    let mut path = fd_path(self.dir.as_fd())?;
    path.push(&self.name);
    Some(path)
  }

  /// Renames this entry to `to`'s.
  pub fn rename_to(&self, to: &CheckedParent) -> io::Result<()> {
    let from = cstr(&self.name)?;
    let to_name = cstr(&to.name)?;
    // SAFETY: valid descriptors and C strings
    cvt(unsafe {
      libc::renameat(
        self.dir.as_raw_fd(),
        from.as_ptr(),
        to.dir.as_raw_fd(),
        to_name.as_ptr(),
      )
    })
  }

  /// Creates a symlink named by this entry that points at `target` (the
  /// target is not checked here: following the link later is).
  pub fn symlink(&self, target: &Path) -> io::Result<()> {
    let target = cstr(target.as_os_str())?;
    let name = cstr(&self.name)?;
    // SAFETY: valid C strings and descriptor
    cvt(unsafe {
      libc::symlinkat(target.as_ptr(), self.dir.as_raw_fd(), name.as_ptr())
    })
  }

  /// Opens the entry itself, never following it if it is a symlink
  /// (`O_NOFOLLOW`, whatever `opts` says).
  pub fn open(&self, opts: &CheckedOpenOptions) -> io::Result<CheckedFile> {
    let fd = openat(
      self.dir.as_fd(),
      &self.name,
      opts.flags() | libc::O_NOFOLLOW,
      opts.mode,
    )?;
    Ok(CheckedFile {
      fd,
      display: self.display.clone(),
    })
  }

  /// `lstat` of the entry.
  pub fn symlink_metadata(&self) -> io::Result<libc::stat> {
    fstatat(self.dir.as_fd(), &self.name, libc::AT_SYMLINK_NOFOLLOW)
  }

  /// The text of the entry, a symlink.
  pub fn read_link(&self) -> io::Result<PathBuf> {
    readlinkat(self.dir.as_fd(), &self.name)
  }

  /// Removes the entry, a directory, with everything in it. Nothing is
  /// followed: each level is opened with `O_NOFOLLOW` from the one above,
  /// so a symlink swapped in for a directory is removed as a link, never
  /// entered.
  pub fn remove_dir_all(&self) -> io::Result<()> {
    remove_tree(self.dir.as_fd(), &self.name, 0)
  }
}

/// How deep [`CheckedParent::remove_dir_all`] descends (each level holds a
/// descriptor).
const MAX_REMOVE_DEPTH: usize = 512;

fn remove_tree(
  parent: BorrowedFd<'_>,
  name: &OsStr,
  depth: usize,
) -> io::Result<()> {
  if depth > MAX_REMOVE_DEPTH {
    return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
  }
  let dir = openat(
    parent,
    name,
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
    0,
  )?;
  for entry in read_dir_fd(dir.as_fd())? {
    let is_dir = fstatat(dir.as_fd(), &entry, libc::AT_SYMLINK_NOFOLLOW)
      .is_ok_and(|st| st.st_mode & libc::S_IFMT == libc::S_IFDIR);
    let removed = if is_dir {
      remove_tree(dir.as_fd(), &entry, depth + 1)
    } else {
      unlinkat(dir.as_fd(), &entry, 0)
    };
    match removed {
      Ok(()) => {}
      // removed by someone else meanwhile
      Err(err) if err.kind() == io::ErrorKind::NotFound => {}
      Err(err) => return Err(err),
    }
  }
  unlinkat(parent, name, libc::AT_REMOVEDIR)
}

/// What a directory entry is, as far as a listing tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
  File,
  Dir,
  Symlink,
  Other,
}

impl CheckedFile {
  /// The entries of an opened directory with their kinds (the entry
  /// itself: a symlink is a symlink).
  pub fn read_dir_kinds(&self) -> io::Result<Vec<(OsString, EntryKind)>> {
    let names = read_dir_fd(self.fd.as_fd())?;
    Ok(
      names
        .into_iter()
        .map(|name| {
          let kind =
            match fstatat(self.fd.as_fd(), &name, libc::AT_SYMLINK_NOFOLLOW) {
              Ok(st) => match st.st_mode & libc::S_IFMT {
                libc::S_IFREG => EntryKind::File,
                libc::S_IFDIR => EntryKind::Dir,
                libc::S_IFLNK => EntryKind::Symlink,
                _ => EntryKind::Other,
              },
              Err(_) => EntryKind::Other,
            };
          (name, kind)
        })
        .collect(),
    )
  }
}

#[derive(Debug, thiserror::Error)]
pub enum CheckedOpenError {
  /// Outside every allow root or inside a deny, as the path resolves now.
  #[error(
    "Requires access to \"{}\": it is outside the allowed scopes or inside a denied one",
    .0.display()
  )]
  NotCapable(PathBuf),
  #[error("{source}: \"{}\"", .path.display())]
  Io {
    path: PathBuf,
    #[source]
    source: io::Error,
  },
}

impl CheckedOpenError {
  pub fn is_not_capable(&self) -> bool {
    matches!(self, CheckedOpenError::NotCapable(_))
  }

  /// The OS error, when it is one.
  pub fn io_error(&self) -> Option<&io::Error> {
    match self {
      CheckedOpenError::Io { source, .. } => Some(source),
      CheckedOpenError::NotCapable(_) => None,
    }
  }
}

fn io_err(path: &Path) -> impl Fn(io::Error) -> CheckedOpenError + '_ {
  move |source| CheckedOpenError::Io {
    path: path.to_path_buf(),
    source,
  }
}

/// Opens `path` (absolute) inside `scope`. See the module docs.
pub fn open_checked(
  scope: &OpenScope,
  path: &Path,
  opts: &CheckedOpenOptions,
) -> Result<CheckedFile, CheckedOpenError> {
  let display = path.to_path_buf();
  let mut path = normalize_absolute(path).ok_or_else(|| relative(path))?;
  // unscoped with nothing denied: nothing to hold the open to (but a
  // path with no link anywhere still takes the walk)
  if scope.roots.is_none()
    && scope.deny.is_empty()
    && opts.symlinks != Symlinks::Never
  {
    let mut flags = opts.flags();
    if opts.symlinks == Symlinks::NoFollowFinal {
      flags |= libc::O_NOFOLLOW;
    }
    let fd = open_path(&path, flags, opts.mode).map_err(io_err(&display))?;
    return Ok(CheckedFile { fd, display });
  }
  let mut hops = 0;
  'restart: loop {
    race_hook(RaceStage::Start);
    if scope.is_denied(&path) {
      return Err(CheckedOpenError::NotCapable(path));
    }
    let (root, root_fd) = scope.root_for(&path)?;
    let rel = relative_components(&path, &root);
    #[cfg(target_os = "linux")]
    {
      if !scope.walk_only
        && !scope.deny_touches(&root)
        && !scope.in_file_root(&root, &path)
        && openat2_available()
      {
        race_hook(RaceStage::BeforeFinal);
        let mut resolve = RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS;
        if opts.symlinks == Symlinks::Never {
          resolve |= RESOLVE_NO_SYMLINKS;
        }
        // RESOLVE_BENEATH holds every link the kernel follows to the root
        let flags = if opts.symlinks == Symlinks::Follow {
          opts.flags()
        } else {
          final_flags(opts)
        };
        match openat2(root_fd.as_fd(), &rel, flags, opts.mode, resolve) {
          Ok(fd) => return Ok(CheckedFile { fd, display }),
          // a link that leaves the root (it may lead to another root), a
          // rename during the lookup, a refused link: the walk decides
          Err(err)
            if matches!(
              err.raw_os_error(),
              Some(libc::EXDEV | libc::EAGAIN | libc::ELOOP)
            ) => {}
          Err(err) => return Err(io_err(&display)(err)),
        }
      }
    }
    let Some((name, dirs)) = rel.split_last() else {
      // the root itself
      let fd = openat(root_fd.as_fd(), OsStr::new("."), final_flags(opts), 0)
        .map_err(io_err(&display))?;
      if !scope.post_open_check(fd.as_fd(), None) {
        return Err(CheckedOpenError::NotCapable(path));
      }
      return Ok(CheckedFile { fd, display });
    };
    let dir = match walk_dirs(&root, root_fd, dirs, name, opts.symlinks)
      .map_err(io_err(&display))?
    {
      Walk::Dir(dir) => dir,
      Walk::Restart(next) => {
        hop(&mut hops, &display)?;
        path = next;
        continue 'restart;
      }
    };
    race_hook(RaceStage::BeforeFinal);
    let opened = match openat(dir.as_fd(), name, final_flags(opts), opts.mode) {
      // a path-only open names a symlink instead of refusing it: when
      // links are followed, read it like a refused one
      Ok(fd)
        if opts.path_only
          && opts.symlinks == Symlinks::Follow
          && is_symlink_fd(fd.as_fd()) =>
      {
        Err(io::Error::from_raw_os_error(libc::ELOOP))
      }
      other => other,
    };
    match opened {
      Ok(fd) => {
        if !scope.post_open_check(fd.as_fd(), None) {
          if opts.create_new {
            // created by this call: take it back
            let _ = unlinkat(dir.as_fd(), name, 0);
          }
          return Err(CheckedOpenError::NotCapable(path));
        }
        return Ok(CheckedFile { fd, display });
      }
      Err(err) if is_link_refusal(&err) && is_symlink_at(dir.as_fd(), name) => {
        if opts.symlinks != Symlinks::Follow {
          return Err(io_err(&display)(io::Error::from_raw_os_error(
            libc::ELOOP,
          )));
        }
        let target = readlinkat(dir.as_fd(), name).map_err(io_err(&display))?;
        hop(&mut hops, &display)?;
        let parent = path.parent().unwrap_or(Path::new("/")).to_path_buf();
        path = join_target(&parent, &target, &[]);
        continue 'restart;
      }
      Err(err) => return Err(io_err(&display)(err)),
    }
  }
}

/// Opens the directory that holds `path`'s last component inside `scope`.
/// The directories on the way may be symlinks (each one checked); the last
/// component is never followed. A path that is an allow root itself gets
/// the root's parent, opened by path as the root is.
pub fn parent_checked(
  scope: &OpenScope,
  path: &Path,
) -> Result<CheckedParent, CheckedOpenError> {
  let display = path.to_path_buf();
  let mut path = normalize_absolute(path).ok_or_else(|| relative(path))?;
  let parent_flags = dir_flags();
  if scope.roots.is_none() && scope.deny.is_empty() {
    let (parent, name) = split_parent(&path).ok_or_else(|| {
      io_err(&display)(io::Error::from_raw_os_error(libc::EINVAL))
    })?;
    let dir = open_path(&parent, parent_flags, 0).map_err(io_err(&display))?;
    return Ok(CheckedParent { dir, name, display });
  }
  let mut hops = 0;
  'restart: loop {
    race_hook(RaceStage::Start);
    if scope.is_denied(&path) {
      return Err(CheckedOpenError::NotCapable(path));
    }
    let (root, root_fd) = scope.root_for(&path)?;
    let rel = relative_components(&path, &root);
    let Some((name, dirs)) = rel.split_last() else {
      // a root itself: its parent, by path, like the root
      let (parent, name) = split_parent(&path).ok_or_else(|| {
        io_err(&display)(io::Error::from_raw_os_error(libc::EINVAL))
      })?;
      let dir =
        open_path(&parent, parent_flags, 0).map_err(io_err(&display))?;
      if !scope.post_open_check(dir.as_fd(), Some(name.as_os_str())) {
        return Err(CheckedOpenError::NotCapable(path));
      }
      return Ok(CheckedParent { dir, name, display });
    };
    #[cfg(target_os = "linux")]
    {
      if !scope.walk_only
        && !scope.deny_touches(&root)
        && !scope.in_file_root(&root, &path)
        && openat2_available()
      {
        race_hook(RaceStage::BeforeFinal);
        let resolve = RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS;
        let rel_dirs = dirs.to_vec();
        match openat2(root_fd.as_fd(), &rel_dirs, parent_flags, 0, resolve) {
          Ok(dir) => {
            return Ok(CheckedParent {
              dir,
              name: name.clone(),
              display,
            });
          }
          Err(err)
            if matches!(
              err.raw_os_error(),
              Some(libc::EXDEV | libc::EAGAIN | libc::ELOOP)
            ) => {}
          Err(err) => return Err(io_err(&display)(err)),
        }
      }
    }
    match walk_dirs(&root, root_fd, dirs, name, Symlinks::NoFollowFinal)
      .map_err(io_err(&display))?
    {
      Walk::Dir(dir) => {
        race_hook(RaceStage::BeforeFinal);
        if !scope.post_open_check(dir.as_fd(), Some(name.as_os_str())) {
          return Err(CheckedOpenError::NotCapable(path));
        }
        return Ok(CheckedParent {
          dir,
          name: name.clone(),
          display,
        });
      }
      Walk::Restart(next) => {
        hop(&mut hops, &display)?;
        path = next;
        continue 'restart;
      }
    }
  }
}

enum Walk {
  /// The directory that holds the last component.
  Dir(OwnedFd),
  /// A directory on the way was a symlink: the whole path as it now reads.
  Restart(PathBuf),
}

/// Walks `dirs` from the root's descriptor, one `O_NOFOLLOW` directory open
/// a component. A symlink among them ends the walk with the path it leads
/// to (`dirs` after it and `name` appended), to be checked from the start.
fn walk_dirs(
  root: &Path,
  root_fd: Arc<OwnedFd>,
  dirs: &[OsString],
  name: &OsStr,
  symlinks: Symlinks,
) -> io::Result<Walk> {
  let mut dir = dup(root_fd.as_fd())?;
  let mut logical = root.to_path_buf();
  for (i, comp) in dirs.iter().enumerate() {
    race_hook(RaceStage::Component(i));
    match openat(dir.as_fd(), comp, dir_flags() | libc::O_NOFOLLOW, 0) {
      Ok(next) => {
        dir = next;
        logical.push(comp);
      }
      Err(err) if is_link_refusal(&err) && is_symlink_at(dir.as_fd(), comp) => {
        if symlinks == Symlinks::Never {
          return Err(io::Error::from_raw_os_error(libc::ELOOP));
        }
        let target = readlinkat(dir.as_fd(), comp)?;
        let mut rest: Vec<&OsStr> =
          dirs[i + 1..].iter().map(|c| c.as_os_str()).collect();
        rest.push(name);
        return Ok(Walk::Restart(join_target(&logical, &target, &rest)));
      }
      Err(err) => return Err(err),
    }
  }
  Ok(Walk::Dir(dir))
}

fn hop(hops: &mut u32, display: &Path) -> Result<(), CheckedOpenError> {
  *hops += 1;
  if *hops > MAX_HOPS {
    return Err(io_err(display)(io::Error::from_raw_os_error(libc::ELOOP)));
  }
  Ok(())
}

fn relative(path: &Path) -> CheckedOpenError {
  io_err(path)(io::Error::new(
    io::ErrorKind::InvalidInput,
    "a checked open takes an absolute path",
  ))
}

/// The flags of the last component's open: never through a link (a link
/// is read and checked by the caller instead).
fn final_flags(opts: &CheckedOpenOptions) -> libc::c_int {
  opts.flags() | libc::O_NOFOLLOW
}

/// A directory used only as the base of `*at` calls.
fn dir_flags() -> libc::c_int {
  #[cfg(target_os = "linux")]
  {
    libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC
  }
  #[cfg(not(target_os = "linux"))]
  {
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC
  }
}

/// What an `O_NOFOLLOW` open of a symlink fails with: `ELOOP`, or
/// `ENOTDIR` with `O_DIRECTORY` (`EMLINK` on the BSDs).
fn is_link_refusal(err: &io::Error) -> bool {
  matches!(
    err.raw_os_error(),
    Some(libc::ELOOP | libc::ENOTDIR | libc::EMLINK)
  )
}

/// The path a link at `at` (a directory) leads to, with `rest` after it,
/// normalized lexically.
fn join_target(at: &Path, target: &Path, rest: &[&OsStr]) -> PathBuf {
  let mut joined = if target.is_absolute() {
    target.to_path_buf()
  } else {
    at.join(target)
  };
  for comp in rest {
    joined.push(comp);
  }
  normalize_absolute(&joined).unwrap_or_else(|| PathBuf::from("/"))
}

/// `path` with `.` and `..` resolved lexically, or `None` when relative.
fn normalize_absolute(path: &Path) -> Option<PathBuf> {
  if !path.is_absolute() {
    return None;
  }
  let mut out = PathBuf::from("/");
  for comp in path.components() {
    match comp {
      Component::RootDir | Component::Prefix(_) | Component::CurDir => {}
      Component::ParentDir => {
        out.pop();
      }
      Component::Normal(c) => out.push(c),
    }
  }
  Some(out)
}

fn relative_components(path: &Path, root: &Path) -> Vec<OsString> {
  path
    .strip_prefix(root)
    .unwrap_or(Path::new(""))
    .components()
    .filter_map(|c| match c {
      Component::Normal(c) => Some(c.to_os_string()),
      _ => None,
    })
    .collect()
}

fn split_parent(path: &Path) -> Option<(PathBuf, OsString)> {
  Some((
    path.parent()?.to_path_buf(),
    path.file_name()?.to_os_string(),
  ))
}

/// Whether `path` is `prefix` or beneath it; case-insensitive on macOS
/// (the default file system there is), which only ever matches more.
fn path_starts_with(path: &Path, prefix: &Path) -> bool {
  #[cfg(target_os = "macos")]
  {
    let mut path = path.components();
    for p in prefix.components() {
      match path.next() {
        Some(c) if c == p => {}
        Some(Component::Normal(c)) => {
          let Component::Normal(p) = p else {
            return false;
          };
          if c.to_string_lossy().to_lowercase()
            != p.to_string_lossy().to_lowercase()
          {
            return false;
          }
        }
        _ => return false,
      }
    }
    true
  }
  #[cfg(not(target_os = "macos"))]
  {
    path.starts_with(prefix)
  }
}

// syscalls

fn cvt(res: libc::c_int) -> io::Result<()> {
  if res < 0 {
    Err(io::Error::last_os_error())
  } else {
    Ok(())
  }
}

fn cstr(s: &OsStr) -> io::Result<CString> {
  CString::new(s.as_bytes()).map_err(|_| {
    io::Error::new(io::ErrorKind::InvalidInput, "a path with a NUL byte")
  })
}

fn open_path(
  path: &Path,
  flags: libc::c_int,
  mode: u32,
) -> io::Result<OwnedFd> {
  let c = cstr(path.as_os_str())?;
  // SAFETY: a valid C string; the descriptor is owned on success
  let fd = unsafe { libc::open(c.as_ptr(), flags, mode as libc::c_uint) };
  if fd < 0 {
    return Err(io::Error::last_os_error());
  }
  // SAFETY: just opened
  Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn open_root(path: &Path) -> io::Result<Arc<OwnedFd>> {
  let fd = match open_path(path, dir_flags(), 0) {
    // macOS: a directory that can be searched but not listed
    #[cfg(target_os = "macos")]
    Err(err) if err.raw_os_error() == Some(libc::EACCES) => {
      open_path(path, O_SEARCH | libc::O_DIRECTORY | libc::O_CLOEXEC, 0)?
    }
    other => other?,
  };
  Ok(Arc::new(fd))
}

/// macOS's `O_EXEC` / `O_SEARCH`: a directory opened to search, not list.
#[cfg(target_os = "macos")]
const O_SEARCH: libc::c_int = 0x4000_0000;

fn openat(
  dir: BorrowedFd<'_>,
  name: &OsStr,
  flags: libc::c_int,
  mode: u32,
) -> io::Result<OwnedFd> {
  let c = cstr(name)?;
  // SAFETY: a valid descriptor and C string; the result is owned
  let fd = unsafe {
    libc::openat(dir.as_raw_fd(), c.as_ptr(), flags, mode as libc::c_uint)
  };
  if fd < 0 {
    return Err(io::Error::last_os_error());
  }
  // SAFETY: just opened
  Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn unlinkat(
  dir: BorrowedFd<'_>,
  name: &OsStr,
  flags: libc::c_int,
) -> io::Result<()> {
  let c = cstr(name)?;
  // SAFETY: a valid descriptor and C string
  cvt(unsafe { libc::unlinkat(dir.as_raw_fd(), c.as_ptr(), flags) })
}

fn dup(fd: BorrowedFd<'_>) -> io::Result<OwnedFd> {
  fd.try_clone_to_owned()
}

fn fstatat(
  dir: BorrowedFd<'_>,
  name: &OsStr,
  flags: libc::c_int,
) -> io::Result<libc::stat> {
  let c = cstr(name)?;
  // SAFETY: zeroed is a valid stat; the call fills it
  let mut st: libc::stat = unsafe { std::mem::zeroed() };
  // SAFETY: a valid descriptor, C string and stat buffer
  cvt(unsafe { libc::fstatat(dir.as_raw_fd(), c.as_ptr(), &mut st, flags) })?;
  Ok(st)
}

fn is_symlink_fd(fd: BorrowedFd<'_>) -> bool {
  // SAFETY: zeroed is a valid stat; the call fills it
  let mut st: libc::stat = unsafe { std::mem::zeroed() };
  // SAFETY: a valid descriptor and stat buffer
  let res = unsafe { libc::fstat(fd.as_raw_fd(), &mut st) };
  res == 0 && st.st_mode & libc::S_IFMT == libc::S_IFLNK
}

fn is_symlink_at(dir: BorrowedFd<'_>, name: &OsStr) -> bool {
  fstatat(dir, name, libc::AT_SYMLINK_NOFOLLOW)
    .is_ok_and(|st| st.st_mode & libc::S_IFMT == libc::S_IFLNK)
}

fn readlinkat(dir: BorrowedFd<'_>, name: &OsStr) -> io::Result<PathBuf> {
  let c = cstr(name)?;
  let mut buf = vec![0u8; libc::PATH_MAX as usize + 1];
  // SAFETY: a valid descriptor, C string and buffer of the given size
  let len = unsafe {
    libc::readlinkat(
      dir.as_raw_fd(),
      c.as_ptr(),
      buf.as_mut_ptr() as *mut libc::c_char,
      buf.len(),
    )
  };
  if len < 0 {
    return Err(io::Error::last_os_error());
  }
  buf.truncate(len as usize);
  Ok(PathBuf::from(OsString::from_vec(buf)))
}

/// The path the kernel holds for an open descriptor, or `None` when it
/// can't be told (no `/proc`, a deleted file).
fn fd_path(fd: BorrowedFd<'_>) -> Option<PathBuf> {
  #[cfg(target_os = "linux")]
  {
    let link = format!("/proc/self/fd/{}", fd.as_raw_fd());
    let c = CString::new(link).ok()?;
    let mut buf = vec![0u8; libc::PATH_MAX as usize + 1];
    // SAFETY: a valid C string and buffer of the given size
    let len = unsafe {
      libc::readlink(
        c.as_ptr(),
        buf.as_mut_ptr() as *mut libc::c_char,
        buf.len(),
      )
    };
    if len <= 0 || len as usize >= buf.len() {
      return None;
    }
    buf.truncate(len as usize);
    if buf.first() != Some(&b'/') || buf.ends_with(b" (deleted)") {
      return None;
    }
    Some(PathBuf::from(OsString::from_vec(buf)))
  }
  #[cfg(target_os = "macos")]
  {
    let mut buf = vec![0u8; libc::PATH_MAX as usize + 1];
    // SAFETY: F_GETPATH writes at most PATH_MAX bytes into the buffer
    let res =
      unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) };
    if res < 0 {
      return None;
    }
    let len = buf.iter().position(|b| *b == 0)?;
    buf.truncate(len);
    Some(PathBuf::from(OsString::from_vec(buf)))
  }
  #[cfg(not(any(target_os = "linux", target_os = "macos")))]
  {
    let _ = fd;
    None
  }
}

/// Where an open descriptor's file is.
#[derive(Debug, PartialEq, Eq)]
pub enum OpenedPath {
  /// The file's own path (no symlink left in it: the kernel reports the
  /// object's location).
  At(PathBuf),
  /// The file has no path any more (unlinked after it was opened).
  Unlinked,
  /// This system can't say (Linux without `/proc`; a platform with no such
  /// query).
  Unknown,
}

/// The path of the file `fd` has open, as the kernel reports it.
pub fn opened_path(fd: BorrowedFd<'_>) -> OpenedPath {
  #[cfg(target_os = "linux")]
  {
    let link = format!("/proc/self/fd/{}", fd.as_raw_fd());
    let Ok(c) = CString::new(link) else {
      return OpenedPath::Unknown;
    };
    let mut buf = vec![0u8; libc::PATH_MAX as usize + 1];
    // SAFETY: a valid C string and buffer of the given size
    let len = unsafe {
      libc::readlink(
        c.as_ptr(),
        buf.as_mut_ptr() as *mut libc::c_char,
        buf.len(),
      )
    };
    if len <= 0 || len as usize >= buf.len() {
      return OpenedPath::Unknown;
    }
    buf.truncate(len as usize);
    if buf.ends_with(b" (deleted)") {
      // a file really named so reads the same: refused with the unlinked
      return OpenedPath::Unlinked;
    }
    if buf.first() != Some(&b'/') {
      // a pipe, a socket, an anonymous file
      return OpenedPath::Unknown;
    }
    OpenedPath::At(PathBuf::from(OsString::from_vec(buf)))
  }
  #[cfg(target_os = "macos")]
  {
    // a file with no links left has no path to report (a pipe or a socket
    // never had one)
    // SAFETY: a zeroed stat the kernel fills
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: a valid descriptor and stat buffer
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } == 0 {
      let kind = st.st_mode & libc::S_IFMT;
      if kind != libc::S_IFREG && kind != libc::S_IFDIR {
        return OpenedPath::Unknown;
      }
      if st.st_nlink == 0 {
        return OpenedPath::Unlinked;
      }
    }
    match fd_path(fd) {
      Some(path) => OpenedPath::At(path),
      None => OpenedPath::Unknown,
    }
  }
  #[cfg(not(any(target_os = "linux", target_os = "macos")))]
  {
    let _ = fd;
    OpenedPath::Unknown
  }
}

fn read_dir_fd(fd: BorrowedFd<'_>) -> io::Result<Vec<OsString>> {
  // fdopendir takes the descriptor over: give it a duplicate
  let raw = dup(fd)?.into_raw_fd();
  // SAFETY: a valid directory descriptor, owned by the DIR on success
  let dirp = unsafe { libc::fdopendir(raw) };
  if dirp.is_null() {
    let err = io::Error::last_os_error();
    // SAFETY: still ours: fdopendir failed
    unsafe { libc::close(raw) };
    return Err(err);
  }
  // SAFETY: rewinding an open DIR; a duplicate shares the offset
  unsafe { libc::rewinddir(dirp) };
  let mut names = Vec::new();
  let result = loop {
    set_errno(0);
    // SAFETY: an open DIR
    let entry = unsafe { libc::readdir(dirp) };
    if entry.is_null() {
      let err = io::Error::last_os_error();
      break match err.raw_os_error() {
        Some(0) | None => Ok(()),
        Some(_) => Err(err),
      };
    }
    // SAFETY: d_name is NUL-terminated
    let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
    let bytes = name.to_bytes();
    if bytes != b"." && bytes != b".." {
      names.push(OsString::from_vec(bytes.to_vec()));
    }
  };
  // SAFETY: closes the DIR and its descriptor
  unsafe { libc::closedir(dirp) };
  result.map(|()| names)
}

#[cfg(target_os = "linux")]
fn set_errno(value: libc::c_int) {
  // SAFETY: the thread's errno location
  unsafe {
    *libc::__errno_location() = value;
  }
}

#[cfg(target_os = "macos")]
fn set_errno(value: libc::c_int) {
  // SAFETY: the thread's errno location
  unsafe {
    *libc::__error() = value;
  }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn set_errno(_value: libc::c_int) {}

// Linux openat2 (5.6+); the number is the same on every architecture
#[cfg(target_os = "linux")]
const NR_OPENAT2: libc::c_long = 437;
#[cfg(target_os = "linux")]
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
#[cfg(target_os = "linux")]
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
#[cfg(target_os = "linux")]
const RESOLVE_BENEATH: u64 = 0x08;

#[cfg(target_os = "linux")]
#[repr(C)]
struct OpenHow {
  flags: u64,
  mode: u64,
  resolve: u64,
}

#[cfg(target_os = "linux")]
fn openat2(
  dir: BorrowedFd<'_>,
  rel: &[OsString],
  flags: libc::c_int,
  mode: u32,
  resolve: u64,
) -> io::Result<OwnedFd> {
  let mut joined = Vec::new();
  for comp in rel {
    if !joined.is_empty() {
      joined.push(b'/');
    }
    joined.extend_from_slice(comp.as_bytes());
  }
  if joined.is_empty() {
    joined.push(b'.');
  }
  let c = CString::new(joined).map_err(|_| {
    io::Error::new(io::ErrorKind::InvalidInput, "a path with a NUL byte")
  })?;
  // the kernel refuses a mode without O_CREAT / O_TMPFILE
  let mode = if flags & libc::O_CREAT != 0 {
    u64::from(mode)
  } else {
    0
  };
  let how = OpenHow {
    flags: flags as u32 as u64,
    mode,
    resolve,
  };
  // SAFETY: a valid descriptor, C string and open_how of the given size
  let fd = unsafe {
    libc::syscall(
      NR_OPENAT2,
      dir.as_raw_fd(),
      c.as_ptr(),
      &how as *const OpenHow,
      std::mem::size_of::<OpenHow>(),
    )
  };
  if fd < 0 {
    return Err(io::Error::last_os_error());
  }
  // SAFETY: just opened
  Ok(unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) })
}

/// Whether `openat2` works here: a kernel before 5.6 says ENOSYS, and a
/// container's seccomp profile may answer EPERM for a syscall it doesn't
/// know.
#[cfg(target_os = "linux")]
fn openat2_available() -> bool {
  static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
  *AVAILABLE.get_or_init(|| {
    let Ok(root) = open_path(Path::new("/"), dir_flags(), 0) else {
      return false;
    };
    match openat2(root.as_fd(), &[], dir_flags(), 0, RESOLVE_BENEATH) {
      Ok(_) => true,
      Err(err) => !matches!(
        err.raw_os_error(),
        Some(libc::ENOSYS | libc::EPERM | libc::EINVAL)
      ),
    }
  })
}

/// The points where a race test swaps something on the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RaceStage {
  /// Before anything is opened (each restart too).
  Start,
  /// Before the walk opens directory component `n`.
  Component(usize),
  /// Before the last component (or, with `openat2`, the whole path) opens.
  BeforeFinal,
}

#[cfg(test)]
type RaceHook = Box<dyn FnMut(RaceStage)>;

#[cfg(test)]
thread_local! {
  static RACE_HOOK: std::cell::RefCell<Option<RaceHook>> =
    const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn race_hook(stage: RaceStage) {
  RACE_HOOK.with(|hook| {
    if let Some(hook) = hook.borrow_mut().as_mut() {
      hook(stage);
    }
  });
}

#[cfg(not(test))]
#[inline(always)]
fn race_hook(_stage: RaceStage) {}

#[cfg(test)]
#[allow(
  clippy::disallowed_methods,
  reason = "the tests build real directory trees"
)]
mod tests {
  use std::fs;
  use std::os::unix::fs::symlink;

  use super::*;

  /// A tree: `<tmp>/allowed` (the root) with `dir/f`, `dir2/`, and
  /// `<tmp>/outside` with the same names holding a secret.
  struct Tree {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    allowed: PathBuf,
    outside: PathBuf,
  }

  fn tree() -> Tree {
    let tmp = tempfile::tempdir().unwrap();
    // canonical, so the post-open path agrees with the roots (macOS /var)
    let base = fs::canonicalize(tmp.path()).unwrap();
    let allowed = base.join("allowed");
    let outside = base.join("outside");
    for d in [&allowed, &outside] {
      fs::create_dir_all(d.join("dir")).unwrap();
      fs::create_dir_all(d.join("dir2")).unwrap();
    }
    fs::write(allowed.join("dir/f"), "checked").unwrap();
    fs::write(outside.join("dir/f"), "secret").unwrap();
    fs::write(outside.join("f"), "secret").unwrap();
    Tree {
      _tmp: tmp,
      base,
      allowed,
      outside,
    }
  }

  /// Both strategies where they exist: `openat2` (Linux) and the walk.
  fn scopes(t: &Tree, deny: Vec<PathBuf>) -> Vec<OpenScope> {
    let mut out = vec![
      OpenScope::new(Some(vec![t.allowed.clone()]), deny.clone())
        .unwrap()
        .force_component_walk(true),
    ];
    if cfg!(target_os = "linux") {
      out.push(OpenScope::new(Some(vec![t.allowed.clone()]), deny).unwrap());
    }
    out
  }

  /// Runs `f` with `hook` called at `stage` (once).
  fn with_hook<R>(
    stage: RaceStage,
    hook: impl FnOnce() + 'static,
    f: impl FnOnce() -> R,
  ) -> R {
    let mut hook = Some(hook);
    RACE_HOOK.with(|h| {
      *h.borrow_mut() = Some(Box::new(move |s| {
        if s == stage
          && let Some(hook) = hook.take()
        {
          hook();
        }
      }));
    });
    let r = f();
    RACE_HOOK.with(|h| *h.borrow_mut() = None);
    r
  }

  /// Swaps `<allowed>/<name>` for a symlink to `target` (the real
  /// directory moves aside to `<name>.moved`).
  fn swap_to_link(allowed: &Path, name: &str, target: &Path) {
    let p = allowed.join(name);
    fs::rename(&p, allowed.join(format!("{name}.moved"))).unwrap();
    symlink(target, &p).unwrap();
  }

  fn read_all(file: CheckedFile) -> String {
    use std::io::Read;
    let mut s = String::new();
    file.into_std_file().read_to_string(&mut s).unwrap();
    s
  }

  const STAGES: [RaceStage; 3] = [
    RaceStage::Start,
    RaceStage::Component(0),
    RaceStage::BeforeFinal,
  ];

  #[test]
  fn opens_inside_and_refuses_outside() {
    let t = tree();
    for scope in scopes(&t, vec![]) {
      let read = CheckedOpenOptions::new().read(true);
      let f = open_checked(&scope, &t.allowed.join("dir/f"), &read).unwrap();
      assert_eq!(f.path(), t.allowed.join("dir/f"));
      assert_eq!(read_all(f), "checked");
      let err =
        open_checked(&scope, &t.outside.join("dir/f"), &read).unwrap_err();
      assert!(err.is_not_capable(), "{err}");
      // `..` is lexical: it can't climb out of the root
      let err =
        open_checked(&scope, &t.allowed.join("dir/../../outside/f"), &read)
          .unwrap_err();
      assert!(err.is_not_capable(), "{err}");
      // the root itself
      let d = open_checked(
        &scope,
        &t.allowed,
        &CheckedOpenOptions::new().read(true).directory(true),
      )
      .unwrap();
      let mut names = d.read_dir().unwrap();
      names.sort();
      assert_eq!(names, vec![OsString::from("dir"), OsString::from("dir2")]);
      // relative paths are refused
      assert!(
        !open_checked(&scope, Path::new("dir/f"), &read)
          .unwrap_err()
          .is_not_capable()
      );
    }
  }

  #[test]
  fn symlinks_inside_and_out() {
    let t = tree();
    symlink(t.allowed.join("dir/f"), t.allowed.join("abs_in")).unwrap();
    symlink("dir/f", t.allowed.join("rel_in")).unwrap();
    symlink(t.outside.join("f"), t.allowed.join("abs_out")).unwrap();
    symlink("../outside/f", t.allowed.join("rel_out")).unwrap();
    symlink(t.outside.join("dir"), t.allowed.join("dir_out")).unwrap();
    symlink("dir", t.allowed.join("dir_in")).unwrap();
    let read = CheckedOpenOptions::new().read(true);
    for scope in scopes(&t, vec![]) {
      for name in ["abs_in", "rel_in", "dir_in/f"] {
        let f = open_checked(&scope, &t.allowed.join(name), &read).unwrap();
        assert_eq!(read_all(f), "checked", "{name}");
      }
      for name in ["abs_out", "rel_out", "dir_out/f"] {
        let err =
          open_checked(&scope, &t.allowed.join(name), &read).unwrap_err();
        assert!(err.is_not_capable(), "{name}: {err}");
      }
      // no-follow kinds
      let nofollow = read.clone().symlinks(Symlinks::NoFollowFinal);
      let err =
        open_checked(&scope, &t.allowed.join("abs_in"), &nofollow).unwrap_err();
      assert_eq!(err.io_error().unwrap().raw_os_error(), Some(libc::ELOOP));
      // links among the directories are fine without Never
      assert!(
        open_checked(&scope, &t.allowed.join("dir_in/f"), &nofollow).is_ok()
      );
      let never = read.clone().symlinks(Symlinks::Never);
      let err =
        open_checked(&scope, &t.allowed.join("dir_in/f"), &never).unwrap_err();
      assert_eq!(err.io_error().unwrap().raw_os_error(), Some(libc::ELOOP));
    }
  }

  // the commonest deny: a file inside the allowed root, reached by name or
  // through a link
  #[test]
  fn deny_inside_allow() {
    let t = tree();
    fs::create_dir(t.allowed.join("denied")).unwrap();
    fs::write(t.allowed.join("denied/secret"), "secret").unwrap();
    fs::write(t.allowed.join(".env"), "secret").unwrap();
    symlink(t.allowed.join("denied/secret"), t.allowed.join("link")).unwrap();
    symlink("denied", t.allowed.join("dlink")).unwrap();
    let deny = vec![t.allowed.join("denied"), t.allowed.join(".env")];
    let read = CheckedOpenOptions::new().read(true);
    for scope in scopes(&t, deny.clone()) {
      for name in ["denied/secret", ".env", "link", "dlink/secret"] {
        let err =
          open_checked(&scope, &t.allowed.join(name), &read).unwrap_err();
        assert!(err.is_not_capable(), "{name}: {err}");
      }
      assert!(open_checked(&scope, &t.allowed.join("dir/f"), &read).is_ok());
      // a name op in the denied directory, also through the link
      for name in ["denied/new", "dlink/new"] {
        let err = parent_checked(&scope, &t.allowed.join(name)).unwrap_err();
        assert!(err.is_not_capable(), "{name}: {err}");
      }
    }
    // unscoped with denies
    let scope = OpenScope::new(None, deny).unwrap();
    assert!(
      open_checked(&scope, &t.allowed.join("link"), &read)
        .unwrap_err()
        .is_not_capable()
    );
    assert!(open_checked(&scope, &t.outside.join("f"), &read).is_ok());
  }

  // open: a directory on the path becomes a link to the outside at every
  // stage; the open refuses or reads the checked file, never the secret
  #[test]
  fn race_open() {
    for stage in STAGES {
      for walk in [true, false] {
        let t = tree();
        let scope = OpenScope::new(Some(vec![t.allowed.clone()]), vec![])
          .unwrap()
          .force_component_walk(walk);
        let (a, o) = (t.allowed.clone(), t.outside.join("dir"));
        let res = with_hook(
          stage,
          move || swap_to_link(&a, "dir", &o),
          || {
            open_checked(
              &scope,
              &t.allowed.join("dir/f"),
              &CheckedOpenOptions::new().read(true),
            )
          },
        );
        match res {
          Ok(f) => assert_eq!(read_all(f), "checked", "{stage:?} {walk}"),
          Err(err) => assert!(err.is_not_capable(), "{stage:?} {walk}: {err}"),
        }
      }
    }
  }

  // open: the last component becomes a link to the outside
  #[test]
  fn race_open_final_component() {
    for walk in [true, false] {
      let t = tree();
      let scope = OpenScope::new(Some(vec![t.allowed.clone()]), vec![])
        .unwrap()
        .force_component_walk(walk);
      let (f, o) = (t.allowed.join("dir/f"), t.outside.join("f"));
      let res = with_hook(
        RaceStage::BeforeFinal,
        move || {
          fs::remove_file(&f).unwrap();
          symlink(&o, &f).unwrap();
        },
        || {
          open_checked(
            &scope,
            &t.allowed.join("dir/f"),
            &CheckedOpenOptions::new().read(true),
          )
        },
      );
      let err = res.unwrap_err();
      assert!(err.is_not_capable(), "{walk}: {err}");
    }
  }

  // create: nothing appears outside
  #[test]
  fn race_create() {
    for stage in STAGES {
      for (walk, new) in [(true, true), (true, false), (false, true)] {
        let t = tree();
        let scope = OpenScope::new(Some(vec![t.allowed.clone()]), vec![])
          .unwrap()
          .force_component_walk(walk);
        let (a, o) = (t.allowed.clone(), t.outside.join("dir"));
        let opts = CheckedOpenOptions::new()
          .write(true)
          .create(!new)
          .create_new(new);
        let res = with_hook(
          stage,
          move || swap_to_link(&a, "dir", &o),
          || open_checked(&scope, &t.allowed.join("dir/new"), &opts),
        );
        assert!(!t.outside.join("dir/new").exists(), "{stage:?} {walk}");
        if let Err(err) = res {
          assert!(err.is_not_capable(), "{stage:?} {walk}: {err}");
        } else {
          // created in the checked directory (moved aside, or, where
          // openat2 skipped the walk's hook, never swapped)
          assert!(
            t.allowed.join("dir.moved/new").exists()
              || t.allowed.join("dir/new").exists()
          );
        }
      }
    }
  }

  // a create through a dangling link that leads outside
  #[test]
  fn create_through_dangling_link() {
    let t = tree();
    symlink(t.outside.join("made"), t.allowed.join("dangling")).unwrap();
    for scope in scopes(&t, vec![]) {
      let opts = CheckedOpenOptions::new().write(true).create(true);
      let err =
        open_checked(&scope, &t.allowed.join("dangling"), &opts).unwrap_err();
      assert!(err.is_not_capable(), "{err}");
      assert!(!t.outside.join("made").exists());
      // create_new never goes through a link at all
      let opts = CheckedOpenOptions::new().write(true).create_new(true);
      let err =
        open_checked(&scope, &t.allowed.join("dangling"), &opts).unwrap_err();
      assert_eq!(err.io_error().unwrap().raw_os_error(), Some(libc::EEXIST));
    }
  }

  /// Runs a name operation on `<allowed>/dir/<name>` with `dir` swapped
  /// for a link to the outside at `stage`.
  fn race_parent(
    stage: RaceStage,
    walk: bool,
    name: &str,
    op: impl FnOnce(&Tree, CheckedParent),
    check: impl FnOnce(&Tree),
  ) {
    let t = tree();
    let scope = OpenScope::new(Some(vec![t.allowed.clone()]), vec![])
      .unwrap()
      .force_component_walk(walk);
    let (a, o) = (t.allowed.clone(), t.outside.join("dir"));
    let res = with_hook(
      stage,
      move || swap_to_link(&a, "dir", &o),
      || parent_checked(&scope, &t.allowed.join("dir").join(name)),
    );
    match res {
      Ok(parent) => op(&t, parent),
      Err(err) => assert!(err.is_not_capable(), "{stage:?} {walk}: {err}"),
    }
    check(&t);
  }

  #[test]
  fn race_mkdir() {
    for stage in STAGES {
      for walk in [true, false] {
        race_parent(
          stage,
          walk,
          "made",
          |_, p| p.mkdir(0o755).unwrap(),
          |t| assert!(!t.outside.join("dir/made").exists(), "{stage:?}"),
        );
      }
    }
  }

  #[test]
  fn race_unlink() {
    for stage in STAGES {
      for walk in [true, false] {
        race_parent(
          stage,
          walk,
          "f",
          |t, p| {
            p.remove_file().unwrap();
            assert!(!t.allowed.join("dir.moved/f").exists());
          },
          |t| assert!(t.outside.join("dir/f").exists(), "{stage:?}"),
        );
      }
    }
  }

  // rename: neither end moves outside when either directory is swapped
  #[test]
  fn race_rename() {
    for stage in STAGES {
      for walk in [true, false] {
        let t = tree();
        let scope = OpenScope::new(Some(vec![t.allowed.clone()]), vec![])
          .unwrap()
          .force_component_walk(walk);
        let from = parent_checked(&scope, &t.allowed.join("dir/f")).unwrap();
        let (a, o) = (t.allowed.clone(), t.outside.join("dir2"));
        let to = with_hook(
          stage,
          move || swap_to_link(&a, "dir2", &o),
          || parent_checked(&scope, &t.allowed.join("dir2/g")),
        );
        match to {
          Ok(to) => {
            from.rename_to(&to).unwrap();
            assert!(
              t.allowed.join("dir2.moved/g").exists()
                || t.allowed.join("dir2/g").exists()
            );
          }
          Err(err) => assert!(err.is_not_capable(), "{stage:?}: {err}"),
        }
        assert!(!t.outside.join("dir2/g").exists(), "{stage:?} {walk}");
        // the source swapped after its check: the rename moves the checked
        // entry, not the outside one
        let t = tree();
        let scope = OpenScope::new(Some(vec![t.allowed.clone()]), vec![])
          .unwrap()
          .force_component_walk(walk);
        let from = parent_checked(&scope, &t.allowed.join("dir/f")).unwrap();
        let to = parent_checked(&scope, &t.allowed.join("dir2/g")).unwrap();
        swap_to_link(&t.allowed, "dir", &t.outside.join("dir"));
        from.rename_to(&to).unwrap();
        assert!(t.outside.join("dir/f").exists());
        assert_eq!(
          fs::read_to_string(t.allowed.join("dir2/g")).unwrap(),
          "checked"
        );
      }
    }
  }

  #[test]
  fn race_readdir() {
    for stage in STAGES {
      for walk in [true, false] {
        let t = tree();
        fs::write(t.outside.join("dir/only_outside"), "").unwrap();
        let scope = OpenScope::new(Some(vec![t.allowed.clone()]), vec![])
          .unwrap()
          .force_component_walk(walk);
        let (a, o) = (t.allowed.clone(), t.outside.join("dir"));
        let res = with_hook(
          stage,
          move || swap_to_link(&a, "dir", &o),
          || {
            open_checked(
              &scope,
              &t.allowed.join("dir"),
              &CheckedOpenOptions::new().read(true).directory(true),
            )
          },
        );
        match res {
          Ok(d) => {
            assert_eq!(d.read_dir().unwrap(), vec![OsString::from("f")]);
          }
          Err(err) => assert!(err.is_not_capable(), "{stage:?} {walk}: {err}"),
        }
      }
    }
  }

  // a deny inside the root holds when a link is swapped in at the last
  // moment (the post-open check on the descriptor)
  #[test]
  fn race_into_deny() {
    let t = tree();
    fs::create_dir(t.allowed.join("denied")).unwrap();
    fs::write(t.allowed.join("denied/secret"), "secret").unwrap();
    let scope = OpenScope::new(
      Some(vec![t.allowed.clone()]),
      vec![t.allowed.join("denied")],
    )
    .unwrap();
    let (a, d) = (t.allowed.clone(), t.allowed.join("denied"));
    let res = with_hook(
      RaceStage::Component(0),
      move || swap_to_link(&a, "dir", &d),
      || {
        open_checked(
          &scope,
          &t.allowed.join("dir/secret"),
          &CheckedOpenOptions::new().read(true),
        )
      },
    );
    assert!(res.unwrap_err().is_not_capable());
    // an object renamed into the deny after the walk held its directory is
    // refused by the post-open check
    let t2 = tree();
    fs::create_dir(t2.allowed.join("denied")).unwrap();
    let scope = OpenScope::new(
      Some(vec![t2.allowed.clone()]),
      vec![t2.allowed.join("denied")],
    )
    .unwrap();
    let (a, d) = (t2.allowed.join("dir"), t2.allowed.join("denied/dir"));
    let res = with_hook(
      RaceStage::BeforeFinal,
      move || fs::rename(&a, &d).unwrap(),
      || {
        open_checked(
          &scope,
          &t2.allowed.join("dir/f"),
          &CheckedOpenOptions::new().read(true),
        )
      },
    );
    assert!(res.unwrap_err().is_not_capable());
  }

  #[test]
  fn link_loops_end() {
    let t = tree();
    symlink(t.allowed.join("b"), t.allowed.join("a")).unwrap();
    symlink(t.allowed.join("a"), t.allowed.join("b")).unwrap();
    for scope in scopes(&t, vec![]) {
      let err = open_checked(
        &scope,
        &t.allowed.join("a"),
        &CheckedOpenOptions::new().read(true),
      )
      .unwrap_err();
      assert_eq!(err.io_error().unwrap().raw_os_error(), Some(libc::ELOOP));
    }
  }

  // two roots: a link from one into the other is followed, checked
  #[test]
  fn link_between_roots() {
    let t = tree();
    let other = t.base.join("other");
    fs::create_dir(&other).unwrap();
    fs::write(other.join("x"), "other").unwrap();
    symlink(other.join("x"), t.allowed.join("to_other")).unwrap();
    for walk in [true, false] {
      let scope =
        OpenScope::new(Some(vec![t.allowed.clone(), other.clone()]), vec![])
          .unwrap()
          .force_component_walk(walk);
      let f = open_checked(
        &scope,
        &t.allowed.join("to_other"),
        &CheckedOpenOptions::new().read(true),
      )
      .unwrap();
      assert_eq!(read_all(f), "other");
    }
  }

  #[test]
  fn parent_ops() {
    let t = tree();
    for scope in scopes(&t, vec![]) {
      let p = parent_checked(&scope, &t.allowed.join("dir/sub")).unwrap();
      assert_eq!(p.name(), "sub");
      p.mkdir(0o755).unwrap();
      assert!(t.allowed.join("dir/sub").is_dir());
      p.remove_dir().unwrap();
      let l = parent_checked(&scope, &t.allowed.join("dir/l")).unwrap();
      l.symlink(&t.outside.join("f")).unwrap();
      let st = l.symlink_metadata().unwrap();
      assert_eq!(st.st_mode & libc::S_IFMT, libc::S_IFLNK);
      // opening the entry never follows it
      assert!(l.open(&CheckedOpenOptions::new().read(true)).is_err());
      l.remove_file().unwrap();
      assert!(t.outside.join("f").exists());
      // a root's own entry: its parent, by path
      let r = parent_checked(&scope, &t.allowed).unwrap();
      assert_eq!(r.name(), "allowed");
      assert!(
        parent_checked(&scope, &t.outside.join("x"))
          .unwrap_err()
          .is_not_capable()
      );
    }
  }
  #[test]
  fn a_file_root_is_that_one_name() {
    let t = tree();
    let file = t.allowed.join("dir/f");
    fs::write(t.allowed.join("dir/sibling"), "sibling").unwrap();
    // a link beside the file root, to its sibling and to the outside
    symlink("sibling", t.allowed.join("dir/to-sibling")).unwrap();
    let opts = CheckedOpenOptions::new().read(true);
    for walk in [true, false] {
      let scope = OpenScope::new(Some(vec![file.clone()]), vec![])
        .unwrap()
        .force_component_walk(walk);
      let opened = open_checked(&scope, &file, &opts).unwrap();
      let mut text = String::new();
      {
        use std::io::Read;
        opened.into_std_file().read_to_string(&mut text).unwrap();
      }
      assert_eq!(text, "checked");
      // nothing else in its folder
      for other in ["dir/sibling", "dir/to-sibling", "dir", "dir2"] {
        assert!(
          open_checked(&scope, &t.allowed.join(other), &opts)
            .unwrap_err()
            .is_not_capable(),
          "{other}"
        );
      }
      // the root replaced by a link to its sibling: followed, refused
      fs::remove_file(&file).unwrap();
      symlink("sibling", &file).unwrap();
      assert!(
        open_checked(&scope, &file, &opts)
          .unwrap_err()
          .is_not_capable()
      );
      fs::remove_file(&file).unwrap();
      fs::write(&file, "checked").unwrap();
    }
  }

  #[test]
  fn custom_flags_keep_the_options_in_charge() {
    let opts = CheckedOpenOptions::new()
      .read(true)
      .custom_flags(libc::O_WRONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    // the access mode stays the options'; O_NOFOLLOW becomes the policy
    assert_eq!(opts.flags() & libc::O_ACCMODE, libc::O_RDONLY);
    assert_eq!(opts.symlinks, Symlinks::NoFollowFinal);
    assert!(opts.flags() & libc::O_NONBLOCK != 0);
    assert_eq!(opts.flags() & libc::O_NOFOLLOW, 0);
    #[cfg(target_os = "linux")]
    {
      let opts = CheckedOpenOptions::new()
        .write(true)
        .custom_flags(libc::O_TMPFILE | libc::O_PATH);
      assert_eq!(opts.flags() & libc::O_PATH, 0);
      assert_eq!(opts.flags() & libc::O_TMPFILE, 0);
      assert_eq!(opts.flags() & libc::O_DIRECTORY, 0);
      let opts = CheckedOpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY);
      assert!(opts.flags() & libc::O_DIRECTORY != 0);
    }
  }

  #[cfg(target_os = "linux")]
  #[test]
  fn a_path_only_open_follows_links_like_any_other() {
    let t = tree();
    symlink("dir/f", t.allowed.join("to-f")).unwrap();
    symlink(t.outside.join("f"), t.allowed.join("to-secret")).unwrap();
    let kind = |file: &CheckedFile| {
      // SAFETY: zeroed is a valid stat; the call fills it
      let mut st: libc::stat = unsafe { std::mem::zeroed() };
      // SAFETY: a valid descriptor and buffer
      unsafe { libc::fstat(file.as_fd().as_raw_fd(), &mut st) };
      st.st_mode & libc::S_IFMT
    };
    for scope in scopes(&t, vec![]) {
      let follow = CheckedOpenOptions::new().path_only(true);
      let file =
        open_checked(&scope, &t.allowed.join("to-f"), &follow).unwrap();
      assert_eq!(kind(&file), libc::S_IFREG);
      assert_eq!(file.real_path().unwrap(), t.allowed.join("dir/f"));
      // the link itself with no-follow
      let nofollow = follow.clone().symlinks(Symlinks::NoFollowFinal);
      let link =
        open_checked(&scope, &t.allowed.join("to-f"), &nofollow).unwrap();
      assert_eq!(kind(&link), libc::S_IFLNK);
      // a link that leaves the scope is never named through
      assert!(
        open_checked(&scope, &t.allowed.join("to-secret"), &follow)
          .unwrap_err()
          .is_not_capable()
      );
    }
  }

  #[test]
  fn remove_dir_all_never_enters_a_link() {
    let t = tree();
    let victim = t.allowed.join("tree");
    fs::create_dir_all(victim.join("a/b")).unwrap();
    fs::write(victim.join("a/b/f"), "x").unwrap();
    symlink(&t.outside, victim.join("a/out")).unwrap();
    symlink(t.outside.join("f"), victim.join("file-link")).unwrap();
    for scope in scopes(&t, vec![]) {
      if !victim.exists() {
        fs::create_dir_all(victim.join("a")).unwrap();
        symlink(&t.outside, victim.join("a/out")).unwrap();
      }
      let parent = parent_checked(&scope, &victim).unwrap();
      let kinds = open_checked(
        &scope,
        &victim,
        &CheckedOpenOptions::new().read(true).directory(true),
      )
      .unwrap()
      .read_dir_kinds()
      .unwrap();
      assert!(kinds.iter().any(|(n, k)| n == "a" && *k == EntryKind::Dir));
      parent.remove_dir_all().unwrap();
      assert!(!victim.exists());
      // what the links pointed at is untouched
      assert_eq!(fs::read_to_string(t.outside.join("f")).unwrap(), "secret");
      assert_eq!(
        fs::read_to_string(t.outside.join("dir/f")).unwrap(),
        "secret"
      );
    }
    // the entry itself swapped for a link to the outside: not entered
    symlink(&t.outside, &victim).unwrap();
    let scope = &scopes(&t, vec![])[0];
    let parent = parent_checked(scope, &victim).unwrap();
    assert!(parent.remove_dir_all().is_err());
    assert!(t.outside.join("f").exists());
  }

  // a root that doesn't exist yet (`--allow-write=subdir/`,
  // `--allow-write=node_modules`, a file to be written) is that one name in
  // its directory until it exists: it can be made, a recursive mkdir can go
  // on beneath it, and nothing else in its directory is reached.
  #[test]
  fn a_missing_root_can_be_created_and_entered() {
    let t = tree();
    for walk_only in [true, false] {
      let new_dir = t.base.join(format!("new-{walk_only}"));
      let scope = OpenScope::new(Some(vec![new_dir.clone()]), Vec::new())
        .unwrap()
        .force_component_walk(walk_only);
      // mkdir of the root itself (one level), then beneath it (two levels)
      parent_checked(&scope, &new_dir)
        .unwrap()
        .mkdir(0o755)
        .unwrap();
      parent_checked(&scope, &new_dir.join("sub"))
        .unwrap()
        .mkdir(0o755)
        .unwrap();
      assert!(new_dir.join("sub").is_dir());
      // a sibling of the root is still out of scope
      assert!(matches!(
        parent_checked(&scope, &t.base.join("other")),
        Err(CheckedOpenError::NotCapable(_))
      ));
      // beneath a root still missing: not found, nothing created
      let missing = t.base.join(format!("missing-{walk_only}"));
      let scope = OpenScope::new(Some(vec![missing.clone()]), Vec::new())
        .unwrap()
        .force_component_walk(walk_only);
      assert!(matches!(
        parent_checked(&scope, &missing.join("a/b")),
        Err(CheckedOpenError::Io { .. })
      ));
      assert!(!missing.exists());
    }
  }

  #[test]
  fn a_missing_file_root_can_be_created() {
    let t = tree();
    let file = t.base.join("out.txt");
    for scope in [
      OpenScope::new(Some(vec![file.clone()]), Vec::new())
        .unwrap()
        .force_component_walk(true),
      OpenScope::new(Some(vec![file.clone()]), Vec::new()).unwrap(),
    ] {
      let _ = fs::remove_file(&file);
      let opts = CheckedOpenOptions::new().write(true).create(true);
      open_checked(&scope, &file, &opts).unwrap();
      assert!(file.is_file());
      assert!(matches!(
        open_checked(&scope, &t.base.join("other.txt"), &opts),
        Err(CheckedOpenError::NotCapable(_))
      ));
    }
  }
}
