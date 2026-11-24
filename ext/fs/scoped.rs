// Copyright 2018-2026 the Deno authors. MIT license.

//! The file system operations of a `Deno.Root` on Unix, on a path held to an
//! [`OpenScope`]. Each one opens the path race-safely
//! ([`deno_permissions::checked_open`]): the open is the check, so a symlink
//! swapped in (by the program, another process or another user) is followed
//! only to a place the scope allows, and the operation acts on the
//! descriptor it got, never on the name again.
//!
//! On macOS the last step of `lstat` (and of a `stat` of a file that can't be
//! opened) is by name, in a folder opened in the scope.

#![allow(
  clippy::disallowed_methods,
  reason = "the real file system's own implementation"
)]

use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use deno_io::fs::FsError;
use deno_io::fs::FsResult;
use deno_io::fs::FsStat;
use deno_permissions::PermissionDeniedError;
use deno_permissions::PermissionState;
use deno_permissions::checked_open::CheckedOpenError;
use deno_permissions::checked_open::CheckedOpenOptions;
use deno_permissions::checked_open::CheckedParent;
use deno_permissions::checked_open::EntryKind;
use deno_permissions::checked_open::OpenScope;
#[cfg(any(target_os = "linux", target_os = "android"))]
use deno_permissions::checked_open::Symlinks;
use deno_permissions::checked_open::open_checked;
use deno_permissions::checked_open::parent_checked;

use crate::FsDirEntry;
use crate::OpenOptions;

fn not_capable(path: &Path) -> FsError {
  FsError::PermissionCheck(
    PermissionDeniedError {
      access: format!("access to \"{}\"", path.display()),
      name: "read",
      custom_message: Some(format!(
        "Requires access to \"{}\": it leads outside the allowed scopes or into a denied one",
        path.display()
      )),
      state: PermissionState::Denied,
    }
    .into(),
  )
}

pub(crate) fn fs_error(err: CheckedOpenError) -> FsError {
  match err {
    CheckedOpenError::NotCapable(path) => not_capable(&path),
    CheckedOpenError::Io { source, .. } => FsError::Io(source),
  }
}

fn io_step<T>(
  path: &Path,
  result: io::Result<T>,
) -> Result<T, CheckedOpenError> {
  result.map_err(|source| CheckedOpenError::Io {
    path: path.to_path_buf(),
    source,
  })
}

fn checked_options(opts: &OpenOptions) -> CheckedOpenOptions {
  let mut checked = CheckedOpenOptions::new()
    .read(opts.read)
    .write(opts.write)
    .append(opts.append)
    // with create_new the file is new: nothing to truncate
    .truncate(opts.truncate && !opts.create_new)
    .create(opts.create)
    .create_new(opts.create_new);
  if let Some(mode) = opts.mode {
    checked = checked.mode(mode & 0o777);
  }
  if let Some(flags) = opts.custom_flags {
    checked = checked.custom_flags(flags);
  }
  checked
}

pub fn open(
  scope: &OpenScope,
  path: &Path,
  opts: OpenOptions,
) -> FsResult<fs::File> {
  // as `std::fs::OpenOptions` has it: truncating takes write access
  // (never append), so a read-only open can't empty a file
  if opts.truncate && !opts.create_new && (!opts.write || opts.append) {
    return Err(FsError::Io(io::Error::from_raw_os_error(libc::EINVAL)));
  }
  open_checked(scope, path, &checked_options(&opts))
    .map(|file| file.into_std_file())
    .map_err(fs_error)
}

/// Creates one directory (its parent must exist).
pub fn mkdir(scope: &OpenScope, path: &Path, mode: u32) -> FsResult<()> {
  parent_checked(scope, path)
    .and_then(|parent| io_step(path, parent.mkdir(mode)))
    .map_err(fs_error)
}

pub fn remove(scope: &OpenScope, path: &Path, recursive: bool) -> FsResult<()> {
  let parent = parent_checked(scope, path).map_err(fs_error)?;
  let is_dir = parent
    .symlink_metadata()
    .map(|st| st.st_mode & libc::S_IFMT == libc::S_IFDIR)?;
  let result = if !is_dir {
    parent.remove_file()
  } else if recursive {
    parent.remove_dir_all()
  } else {
    parent.remove_dir()
  };
  Ok(result?)
}

pub fn rename(
  from_scope: &OpenScope,
  from: &Path,
  to_scope: &OpenScope,
  to: &Path,
) -> FsResult<()> {
  two(from_scope, from, to_scope, to)
    .and_then(|(a, b)| io_step(from, a.rename_to(&b)))
    .map_err(fs_error)
}

fn two(
  from_scope: &OpenScope,
  from: &Path,
  to_scope: &OpenScope,
  to: &Path,
) -> Result<(CheckedParent, CheckedParent), CheckedOpenError> {
  Ok((
    parent_checked(from_scope, from)?,
    parent_checked(to_scope, to)?,
  ))
}

pub fn symlink(scope: &OpenScope, target: &Path, link: &Path) -> FsResult<()> {
  parent_checked(scope, link)
    .and_then(|parent| io_step(link, parent.symlink(target)))
    .map_err(fs_error)
}

/// An open descriptor of the object at `path` (links followed) to read its
/// metadata through: read-only, or write-only for a file that can't be
/// read. Never waits (a FIFO).
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn metadata_fd(
  scope: &OpenScope,
  path: &Path,
) -> Result<fs::File, CheckedOpenError> {
  let opts = CheckedOpenOptions::new().read(true).nonblocking(true);
  match open_checked(scope, path, &opts) {
    Err(CheckedOpenError::Io { source, .. })
      if source.raw_os_error() == Some(libc::EACCES) =>
    {
      let opts = CheckedOpenOptions::new().write(true).nonblocking(true);
      match open_checked(scope, path, &opts) {
        Ok(file) => Ok(file.into_std_file()),
        // the first refusal is the one to report
        Err(_) => Err(CheckedOpenError::Io {
          path: path.to_path_buf(),
          source,
        }),
      }
    }
    other => other.map(|file| file.into_std_file()),
  }
}

/// `stat`: the object the path leads to, links followed (each one checked).
pub fn stat(scope: &OpenScope, path: &Path) -> FsResult<FsStat> {
  // Linux names the object without opening it, so no right on the file
  // itself is needed, as for stat(2)
  #[cfg(any(target_os = "linux", target_os = "android"))]
  {
    let opts = CheckedOpenOptions::new().path_only(true);
    let file = open_checked(scope, path, &opts).map_err(fs_error)?;
    Ok(FsStat::from_std(file.into_std_file().metadata()?))
  }
  #[cfg(not(any(target_os = "linux", target_os = "android")))]
  {
    match metadata_fd(scope, path) {
      Ok(file) => Ok(FsStat::from_std(file.metadata()?)),
      Err(CheckedOpenError::NotCapable(path)) => Err(not_capable(&path)),
      // an object that can't be opened (no right on it, a socket): its
      // folder is checked, the last step is by name
      Err(CheckedOpenError::Io { source, .. })
        if matches!(
          source.raw_os_error(),
          Some(libc::EACCES | libc::EPERM | libc::EOPNOTSUPP | libc::ENXIO)
        ) =>
      {
        let real = checked_name(scope, path)?;
        Ok(FsStat::from_std(fs::metadata(real)?))
      }
      Err(err) => Err(fs_error(err)),
    }
  }
}

/// `lstat`: the entry itself.
pub fn lstat(scope: &OpenScope, path: &Path) -> FsResult<FsStat> {
  #[cfg(any(target_os = "linux", target_os = "android"))]
  {
    let opts = CheckedOpenOptions::new()
      .path_only(true)
      .symlinks(Symlinks::NoFollowFinal);
    let file = open_checked(scope, path, &opts).map_err(fs_error)?;
    Ok(FsStat::from_std(file.into_std_file().metadata()?))
  }
  #[cfg(not(any(target_os = "linux", target_os = "android")))]
  {
    let real = checked_name(scope, path)?;
    Ok(FsStat::from_std(fs::symlink_metadata(real)?))
  }
}

/// The entry's folder opened in the scope, and the entry's name there as
/// the kernel spells the folder now (for a last step that is by name).
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn checked_name(scope: &OpenScope, path: &Path) -> FsResult<PathBuf> {
  let parent = parent_checked(scope, path).map_err(fs_error)?;
  parent.real_path().ok_or_else(|| not_capable(path))
}

pub fn read_link(scope: &OpenScope, path: &Path) -> FsResult<PathBuf> {
  let parent = parent_checked(scope, path).map_err(fs_error)?;
  Ok(parent.read_link()?)
}

pub fn read_dir(scope: &OpenScope, path: &Path) -> FsResult<Vec<FsDirEntry>> {
  let opts = CheckedOpenOptions::new().read(true).directory(true);
  let dir = open_checked(scope, path, &opts).map_err(fs_error)?;
  Ok(
    dir
      .read_dir_kinds()?
      .into_iter()
      .map(|(name, kind)| FsDirEntry {
        // decoded lossily, as the path-based listing does
        name: name.to_string_lossy().into_owned(),
        is_file: kind == EntryKind::File,
        is_directory: kind == EntryKind::Dir,
        is_symlink: kind == EntryKind::Symlink,
      })
      .collect(),
  )
}

#[cfg(test)]
mod tests {
  use std::os::unix::fs::symlink as make_link;

  use super::*;

  /// `<tmp>/allowed` (the root) holding a denied `secret` folder, links to
  /// it and to `<tmp>/outside`.
  struct Bed {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    scope: OpenScope,
  }

  fn bed() -> Bed {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let a = root.join("allowed");
    fs::create_dir_all(a.join("sub")).unwrap();
    fs::create_dir_all(a.join("secret")).unwrap();
    fs::create_dir_all(root.join("outside")).unwrap();
    fs::write(a.join("file"), "file").unwrap();
    fs::write(a.join("secret/key"), "key").unwrap();
    fs::write(root.join("outside/data"), "data").unwrap();
    make_link(root.join("outside"), a.join("to-outside")).unwrap();
    make_link("secret", a.join("to-secret")).unwrap();
    make_link("file", a.join("to-file")).unwrap();
    let scope =
      OpenScope::new(Some(vec![a.clone()]), vec![a.join("secret")]).unwrap();
    Bed {
      _tmp: tmp,
      root,
      scope,
    }
  }

  impl Bed {
    fn p(&self, rel: &str) -> PathBuf {
      self.root.join(rel)
    }
  }

  fn refused<T>(result: FsResult<T>) -> bool {
    matches!(result, Err(FsError::PermissionCheck(_)))
  }

  fn write_opts() -> OpenOptions {
    OpenOptions {
      read: false,
      write: true,
      create: true,
      truncate: true,
      append: false,
      create_new: false,
      custom_flags: None,
      mode: None,
    }
  }

  /// Every path that leads out of the scope or into the deny, by name and
  /// through a link in the allowed folder.
  const ESCAPES: &[&str] = &[
    "outside/data",
    "allowed/to-outside/data",
    "allowed/secret/key",
    "allowed/to-secret/key",
  ];

  #[test]
  fn opens_stay_in_the_scope() {
    let b = bed();
    let mut file = open(&b.scope, &b.p("allowed/new"), write_opts()).unwrap();
    {
      use std::io::Write;
      file.write_all(b"new").unwrap();
    }
    assert_eq!(fs::read(b.p("allowed/new")).unwrap(), b"new");
    // through a link that stays inside
    let read = OpenOptions::read();
    let mut text = String::new();
    {
      use std::io::Read;
      open(&b.scope, &b.p("allowed/to-file"), read)
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    }
    assert_eq!(text, "file");
    for path in ESCAPES {
      assert!(refused(open(&b.scope, &b.p(path), read)), "{path}");
      assert!(refused(open(&b.scope, &b.p(path), write_opts())), "{path}");
    }
    assert_eq!(fs::read(b.p("outside/data")).unwrap(), b"data");
    assert_eq!(fs::read(b.p("allowed/secret/key")).unwrap(), b"key");
    // O_NOFOLLOW refuses a link as the last component
    let nofollow = OpenOptions {
      custom_flags: Some(libc::O_NOFOLLOW),
      ..read
    };
    let err = open(&b.scope, &b.p("allowed/to-file"), nofollow).unwrap_err();
    assert_eq!(err.into_io_error().raw_os_error(), Some(libc::ELOOP));
    // truncation needs write access
    let read_truncate = OpenOptions {
      truncate: true,
      ..read
    };
    let err = open(&b.scope, &b.p("allowed/file"), read_truncate).unwrap_err();
    assert_eq!(err.into_io_error().raw_os_error(), Some(libc::EINVAL));
    assert_eq!(fs::read(b.p("allowed/file")).unwrap(), b"file");
  }

  #[test]
  fn name_operations_stay_in_the_scope() {
    let b = bed();
    let s = &b.scope;
    mkdir(s, &b.p("allowed/a"), 0o755).unwrap();
    mkdir(s, &b.p("allowed/a/b"), 0o755).unwrap();
    assert!(b.p("allowed/a/b").is_dir());
    assert!(mkdir(s, &b.p("allowed/a/b"), 0o755).is_err());
    fs::write(b.p("allowed/a/b/f"), "x").unwrap();
    rename(s, &b.p("allowed/a/b/f"), s, &b.p("allowed/a/g")).unwrap();
    symlink(s, Path::new("g"), &b.p("allowed/a/l")).unwrap();
    assert_eq!(
      read_link(s, &b.p("allowed/a/l")).unwrap(),
      PathBuf::from("g")
    );
    let listed = read_dir(s, &b.p("allowed/a")).unwrap();
    let kind = |name: &str| {
      let e = listed.iter().find(|e| e.name == name).unwrap();
      (e.is_file, e.is_directory, e.is_symlink)
    };
    assert_eq!(kind("g"), (true, false, false));
    assert_eq!(kind("b"), (false, true, false));
    assert_eq!(kind("l"), (false, false, true));
    assert!(stat(s, &b.p("allowed/a/l")).unwrap().is_file);
    assert!(lstat(s, &b.p("allowed/a/l")).unwrap().is_symlink);
    remove(s, &b.p("allowed/a/b"), false).unwrap();
    // a link to the outside inside the tree: removed as a link
    make_link(b.p("outside"), b.p("allowed/a/out")).unwrap();
    remove(s, &b.p("allowed/a"), true).unwrap();
    assert!(!b.p("allowed/a").exists());
    assert!(b.p("outside/data").exists());
    for path in ESCAPES {
      let path = b.p(path);
      assert!(refused(remove(s, &path, false)), "{path:?}");
      assert!(refused(stat(s, &path)), "{path:?}");
      assert!(refused(lstat(s, &path)), "{path:?}");
      assert!(refused(read_link(s, &path)), "{path:?}");
      assert!(refused(rename(s, &path, s, &b.p("allowed/x"))), "{path:?}");
      assert!(
        refused(rename(s, &b.p("allowed/file"), s, &path)),
        "{path:?}"
      );
      assert!(refused(symlink(s, Path::new("t"), &path)), "{path:?}");
    }
    for dir in ["outside", "allowed/to-outside", "allowed/secret"] {
      assert!(refused(read_dir(s, &b.p(dir))), "{dir}");
      assert!(refused(mkdir(s, &b.p(dir).join("made"), 0o755)), "{dir}");
      assert!(!b.p(dir).join("made").exists());
    }
    assert_eq!(fs::read(b.p("outside/data")).unwrap(), b"data");
    assert_eq!(fs::read(b.p("allowed/secret/key")).unwrap(), b"key");
  }
}
