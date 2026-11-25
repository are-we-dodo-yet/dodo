// Copyright 2018-2026 the Deno authors. MIT license.

use deno_runtime::deno_core::HmrUpdateOutcome;
use deno_runtime::deno_core::error::CoreError;
use deno_runtime::deno_core::futures::future::Either;
use deno_runtime::deno_core::futures::future::LocalBoxFuture;
use deno_runtime::deno_core::futures::future::select;

use crate::worker::LibMainWorker;

pub enum HmrChange {
  Update(Vec<String>),
  FullReload(String),
}

pub trait HmrHost {
  fn next_change(&mut self) -> LocalBoxFuture<'_, HmrChange>;
  fn on_unloaded_changed(&mut self, specifiers: &[String]) -> Option<String>;
  fn on_updated(&mut self, updated: &[String]);
  fn on_pruned(&mut self, _pruned: &[String]) {}
  fn full_reload(&mut self, reason: &str);
}

impl LibMainWorker {
  pub fn enable_hmr(&mut self) {
    self.js_runtime().enable_hmr();
  }

  pub async fn run_event_loop_with_hmr(
    &mut self,
    host: &mut dyn HmrHost,
  ) -> Result<(), CoreError> {
    loop {
      let change = {
        let event_loop = std::pin::pin!(self.run_event_loop(false));
        match select(host.next_change(), event_loop).await {
          Either::Left((change, _)) => change,
          Either::Right((result, _)) => return result,
        }
      };
      match change {
        HmrChange::FullReload(reason) => host.full_reload(&reason),
        HmrChange::Update(specifiers) => {
          let (specifiers, unloaded): (Vec<_>, Vec<_>) = specifiers
            .into_iter()
            .partition(|s| self.js_runtime().hmr_is_module_loaded(s));
          if !unloaded.is_empty()
            && let Some(reason) = host.on_unloaded_changed(&unloaded)
          {
            host.full_reload(&reason);
            continue;
          }
          if specifiers.is_empty() {
            continue;
          }
          match self.js_runtime().hmr_apply_update(specifiers).await {
            Ok(HmrUpdateOutcome::Handled { updated, pruned }) => {
              host.on_pruned(&pruned);
              host.on_updated(&updated)
            }
            Ok(HmrUpdateOutcome::NeedsFullReload { reason }) => {
              host.full_reload(&reason)
            }
            Err(err) => host.full_reload(&format!("update failed: {err}")),
          }
        }
      }
    }
  }
}
