// Copyright 2018-2026 the Deno authors. MIT license.

//! Hot module replacement host for the standalone/desktop runtime.
//!
//! Watches the project's source files on disk and feeds changes into the
//! shared `import.meta.hot` driver in `deno_lib`.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use deno_core::futures::FutureExt;
use deno_core::futures::future::LocalBoxFuture;
use deno_core::url::Url;
use deno_error::JsErrorBox;
use deno_lib::hmr::HmrChange;
use deno_lib::hmr::HmrHost;
use notify::RecursiveMode;
use notify::Watcher;
use notify::event::ModifyKind;
use tokio::sync::mpsc;

/// Coalesce events that arrive within this window into one update. Editors
/// that save-then-format (or do atomic-rename saves) emit several events per
/// keystroke.
const HMR_DEBOUNCE: Duration = Duration::from_millis(50);

pub const HMR_RESTART_EXIT_CODE: i32 = 75;

/// Callback invoked when the webviews should re-fetch their content.
pub type HmrReloadCallback = Box<dyn Fn() + Send + Sync>;

/// What happened to a watched source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileChange {
  Updated,
  Removed,
}

pub struct DesktopHmrHost {
  changed_rx: mpsc::UnboundedReceiver<(PathBuf, FileChange)>,
  pending: HashMap<PathBuf, FileChange>,
  /// The directory being watched on disk (original source location).
  watch_dir: PathBuf,
  /// The VFS root path inside the compiled binary. Script URLs use this base.
  vfs_root: PathBuf,
  _watcher: notify::RecommendedWatcher,
  on_reload: Option<HmrReloadCallback>,
}

impl DesktopHmrHost {
  /// `watch_dir` is the original source directory on disk, `vfs_root` is the
  /// embedded root path that modules are registered under.
  pub fn new(
    watch_dir: PathBuf,
    vfs_root: PathBuf,
    on_reload: Option<HmrReloadCallback>,
  ) -> Result<Self, JsErrorBox> {
    let (changed_tx, changed_rx) = mpsc::unbounded_channel();

    let mut watcher =
      notify::recommended_watcher(move |res: Result<notify::Event, _>| {
        let Ok(event) = res else {
          return;
        };
        // Metadata-only modifications (chmod, touch) are pure noise.
        let change = match event.kind {
          notify::EventKind::Create(_) => FileChange::Updated,
          notify::EventKind::Modify(ModifyKind::Metadata(_)) => return,
          notify::EventKind::Modify(_) => FileChange::Updated,
          notify::EventKind::Remove(_) => FileChange::Removed,
          _ => return,
        };
        for path in event.paths {
          if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && matches!(
              ext,
              "js" | "ts" | "jsx" | "tsx" | "mjs" | "cjs" | "mts" | "cts"
            )
          {
            let _ = changed_tx.send((path, change));
          }
        }
      })
      .map_err(|e| JsErrorBox::generic(e.to_string()))?;

    watcher
      .watch(&watch_dir, RecursiveMode::Recursive)
      .map_err(|e| JsErrorBox::generic(e.to_string()))?;

    #[allow(
      clippy::disallowed_methods,
      reason = "denort has no canonicalize helper; falls back to the original path"
    )]
    let watch_dir_canonical = watch_dir
      .canonicalize()
      .unwrap_or_else(|_| watch_dir.clone());

    Ok(Self {
      changed_rx,
      pending: HashMap::new(),
      watch_dir: watch_dir_canonical,
      vfs_root,
      _watcher: watcher,
      on_reload,
    })
  }

  fn module_url(&self, path: &Path, change: FileChange) -> Option<String> {
    #[allow(
      clippy::disallowed_methods,
      reason = "denort has no canonicalize helper; non-canonicalizable paths are handled below"
    )]
    let canonical = match (change, path.canonicalize()) {
      (FileChange::Updated, Ok(p)) => p,
      (FileChange::Updated, Err(_)) => return None,
      // Removed paths can't be canonicalized; reconstruct best-effort.
      (FileChange::Removed, _) => path.to_path_buf(),
    };
    let relative = canonical.strip_prefix(&self.watch_dir).ok()?;
    Url::from_file_path(self.vfs_root.join(relative))
      .ok()
      .map(String::from)
  }

  async fn next_change_inner(&mut self) -> HmrChange {
    loop {
      if self.pending.is_empty() {
        match self.changed_rx.recv().await {
          Some((path, change)) => {
            self.pending.insert(path, change);
          }
          None => std::future::pending().await,
        }
      }
      // Coalesce a burst of events into a single pass, with the latest kind
      // per path winning. Ends as soon as the channel quiets down for
      // HMR_DEBOUNCE.
      while let Ok(Some((path, change))) =
        tokio::time::timeout(HMR_DEBOUNCE, self.changed_rx.recv()).await
      {
        self.pending.insert(path, change);
      }

      let pending = std::mem::take(&mut self.pending);
      let mut specifiers = Vec::with_capacity(pending.len());
      let mut is_valid = true;
      for (path, change) in pending {
        let Some(specifier) = self.module_url(&path, change) else {
          continue;
        };
        log::debug!("HMR: {:?} {} -> {}", change, path.display(), specifier);
        if change == FileChange::Updated
          && let Err(err) = check_syntax(&path, &specifier)
        {
          log::error!("HMR: update not applied: {}", err);
          is_valid = false;
        }
        specifiers.push(specifier);
      }
      if is_valid && !specifiers.is_empty() {
        specifiers.sort();
        return HmrChange::Update(specifiers);
      }
    }
  }

  fn reload_webviews(&self) {
    if let Some(on_reload) = &self.on_reload {
      on_reload();
    }
  }
}

fn check_syntax(path: &Path, specifier: &str) -> Result<(), JsErrorBox> {
  let text = std::fs::read_to_string(path).map_err(JsErrorBox::from_err)?;
  let specifier = Url::parse(specifier).map_err(JsErrorBox::from_err)?;
  let media_type = deno_media_type::MediaType::from_specifier(&specifier);
  deno_ast::parse_module(deno_ast::ParseParams {
    specifier,
    text: text.into(),
    media_type,
    capture_tokens: false,
    scope_analysis: false,
    maybe_syntax: None,
  })
  .map_err(JsErrorBox::from_err)?;
  Ok(())
}

impl HmrHost for DesktopHmrHost {
  fn next_change(&mut self) -> LocalBoxFuture<'_, HmrChange> {
    self.next_change_inner().boxed_local()
  }

  fn on_unloaded_changed(&mut self, _specifiers: &[String]) -> Option<String> {
    self.reload_webviews();
    None
  }

  fn on_updated(&mut self, updated: &[String]) {
    for specifier in updated {
      log::info!("HMR: updated {}", specifier);
    }
    self.reload_webviews();
  }

  #[allow(
    clippy::disallowed_methods,
    reason = "the desktop supervisor relaunches the app on this exit code"
  )]
  fn full_reload(&mut self, reason: &str) {
    log::info!("HMR: {}, restarting", reason);
    std::process::exit(HMR_RESTART_EXIT_CODE);
  }
}
