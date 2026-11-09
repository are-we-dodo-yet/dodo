// Copyright 2018-2026 the Deno authors. MIT license.

use deno_core::error::AnyError;

mod analysis;
mod cache;
mod capabilities;
mod client;
mod code_lens;
mod compiler_options;
mod completions;
mod config;
mod diagnostics;
mod documents;
mod jsr;
pub mod language_server;
mod lint;
mod logging;
mod lsp_custom;
mod npm;
mod parent_process_checker;
mod path_to_regex;
mod performance;
mod refactor;
mod registries;
mod resolver;
mod search;
mod semantic_tokens;
mod server;
mod test_code_actions;
mod testing;
mod text;
mod trace;
mod ts_server;
mod tsc;
mod urls;

pub async fn start() -> Result<(), AnyError> {
  server::serve().await
}
