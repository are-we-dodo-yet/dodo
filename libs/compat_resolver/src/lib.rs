// Copyright 2018-2026 the Deno authors. MIT license.

use anyhow::Context as _;
use bun_lockfile_import::bun_lock_to_deno_lock_v5;
use deno_lockfile::Lockfile;
use npm_lockfile_import::package_lock_to_deno_lock_v5;
use pnpm_lockfile_import::pnpm_lock_to_deno_lock_v5;
use sys_traits::FsRead;
use yarn_lockfile_import::yarn_lock_to_deno_lock_v5;

mod bun_lockfile_import;
mod npm_lockfile_import;
mod pnpm_lockfile_import;
mod yarn_lockfile_import;

/// Attempt to translate a sibling `package-lock.json`, `pnpm-lock.yaml`,
/// `yarn.lock`, or `bun.lock` into a seed `Lockfile`. Returns `Ok(None)` when
/// no usable lockfile is present (so the caller falls back to creating an empty
/// lockfile). The returned lockfile is flagged as changed so the next write
/// persists it to disk.
///
/// When several are present, the first in the order
/// `package-lock.json`, `pnpm-lock.yaml`, `yarn.lock`, `bun.lock` wins.
pub async fn import_npm_lockfile<TSys: FsRead>(
  sys: &TSys,
  deno_lock_path: &std::path::Path,
  api: &dyn deno_lockfile::NpmPackageInfoProvider,
) -> anyhow::Result<Option<Lockfile>> {
  let Some(parent) = deno_lock_path.parent() else {
    return Ok(None);
  };

  type Translator = fn(&str) -> Result<String, String>;
  let candidates: [(&str, Translator); 4] = [
    ("package-lock.json", |s| {
      package_lock_to_deno_lock_v5(s).map_err(|e| e.to_string())
    }),
    ("pnpm-lock.yaml", |s| {
      pnpm_lock_to_deno_lock_v5(s).map_err(|e| e.to_string())
    }),
    ("yarn.lock", |s| {
      yarn_lock_to_deno_lock_v5(s).map_err(|e| e.to_string())
    }),
    ("bun.lock", |s| {
      bun_lock_to_deno_lock_v5(s).map_err(|e| e.to_string())
    }),
  ];

  for (file_name, translate) in candidates {
    let path = parent.join(file_name);
    let text = match sys.fs_read_to_string(&path) {
      Ok(text) => text,
      Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
      Err(err) => {
        return Err(err)
          .with_context(|| format!("Failed reading '{}'", path.display()));
      }
    };
    let deno_lock_text = match translate(&text) {
      Ok(text) => text,
      Err(err) => {
        log::warn!(
          "Failed to import {} at {}: {}. Trying the next candidate.",
          file_name,
          path.display(),
          err
        );
        continue;
      }
    };
    let mut lockfile = Lockfile::new(
      deno_lockfile::NewLockfileOptions {
        file_path: deno_lock_path.to_path_buf(),
        content: &deno_lock_text,
        overwrite: false,
      },
      api,
    )
    .await?;
    // Only announce the import when the translation actually produced
    // content. A foreign lockfile whose deps are all unsupported (e.g. only
    // `file:`/`link:` entries) translates to an empty lockfile, and claiming
    // we seeded it would be misleading.
    if !lockfile.content.is_empty() {
      log::info!("Seeded deno.lock from {}", path.display());
    }
    // Force write on first save so the imported state is persisted even if
    // no subsequent resolution mutates the lockfile.
    lockfile.has_content_changed = true;
    return Ok(Some(lockfile));
  }

  Ok(None)
}
