// Copyright 2018-2026 the Deno authors. MIT license.

use std::path::PathBuf;
use std::sync::Arc;

use deno_core::futures::FutureExt;
use deno_core::futures::future::LocalBoxFuture;
use deno_lib::hmr::HmrChange;
use deno_lib::hmr::HmrHost;
use deno_terminal::colors;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::broadcast::error::TryRecvError;

use crate::graph_container::HmrReload;
use crate::graph_container::MainModuleGraphContainer;
use crate::util::file_watcher::WatcherCommunicator;
use crate::util::file_watcher::WatcherRestartMode;

pub struct CliHmrHost {
  watcher_communicator: Arc<WatcherCommunicator>,
  graph_container: MainModuleGraphContainer,
  changed_paths_rx: broadcast::Receiver<Option<Vec<PathBuf>>>,
  pending: Option<Vec<PathBuf>>,
}

impl CliHmrHost {
  pub fn new(
    watcher_communicator: Arc<WatcherCommunicator>,
    graph_container: MainModuleGraphContainer,
  ) -> Self {
    let changed_paths_rx = watcher_communicator.subscribe_changed_paths();
    Self {
      watcher_communicator,
      graph_container,
      changed_paths_rx,
      pending: None,
    }
  }

  pub fn start(&self) {
    self
      .watcher_communicator
      .change_restart_mode(WatcherRestartMode::Manual);
  }

  pub fn stop(&self) {
    self
      .watcher_communicator
      .change_restart_mode(WatcherRestartMode::Automatic);
  }

  async fn next_change_inner(&mut self) -> HmrChange {
    loop {
      if self.pending.is_none() {
        match self.changed_paths_rx.recv().await {
          Ok(Some(paths)) => self.pending = Some(paths),
          Ok(None) => {
            return HmrChange::FullReload("unknown file change".to_string());
          }
          Err(RecvError::Lagged(_)) => {
            return HmrChange::FullReload(
              "too many file changes to track".to_string(),
            );
          }
          Err(RecvError::Closed) => std::future::pending().await,
        }
      }
      loop {
        match self.changed_paths_rx.try_recv() {
          Ok(Some(paths)) => {
            let pending = self.pending.as_mut().unwrap();
            for path in paths {
              if !pending.contains(&path) {
                pending.push(path);
              }
            }
          }
          Ok(None) | Err(TryRecvError::Lagged(_)) => {
            self.pending = None;
            return HmrChange::FullReload("unknown file change".to_string());
          }
          Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => break,
        }
      }
      let paths = self.pending.clone().unwrap();
      if paths.is_empty() {
        self.pending = None;
        return HmrChange::FullReload("a watched file was removed".to_string());
      }
      let result = self.graph_container.hmr_reload_paths(&paths).await;
      self.pending = None;
      match result {
        Ok(HmrReload::Reloaded(specifiers)) => {
          return HmrChange::Update(
            specifiers.into_iter().map(String::from).collect(),
          );
        }
        Ok(HmrReload::NotInGraph(path)) => {
          return HmrChange::FullReload(format!(
            "{} is not a module in the graph",
            path.display()
          ));
        }
        Err(err) => {
          log::error!("{}: {}", colors::red_bold("error"), err);
          self.watcher_communicator.print(
            "Update not applied. Waiting for file changes...".to_string(),
          );
        }
      }
    }
  }
}

impl HmrHost for CliHmrHost {
  fn next_change(&mut self) -> LocalBoxFuture<'_, HmrChange> {
    self.next_change_inner().boxed_local()
  }

  fn on_unloaded_changed(&mut self, specifiers: &[String]) -> Option<String> {
    Some(format!("{} is not a loaded module", specifiers[0]))
  }

  fn on_updated(&mut self, updated: &[String]) {
    for specifier in updated {
      self
        .watcher_communicator
        .print(format!("Updated {}", specifier));
    }
  }

  fn on_pruned(&mut self, pruned: &[String]) {
    for specifier in pruned {
      self
        .watcher_communicator
        .print(format!("Pruned {}", specifier));
    }
  }

  fn full_reload(&mut self, reason: &str) {
    log::debug!("HMR full reload: {}", reason);
    let _ = self.watcher_communicator.force_restart();
  }
}
