// Copyright 2018-2026 the Deno authors. MIT license.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use deno_ast::ModuleSpecifier;
use deno_config::glob::FilePatterns;
use deno_config::glob::PathOrPatternSet;
use deno_core::error::AnyError;
use deno_core::parking_lot::RwLock;
use deno_graph::Module;
use deno_graph::ModuleGraph;
use deno_runtime::deno_permissions::PermissionsContainer;

use crate::args::CliOptions;
use crate::graph_util::GraphRootsValidOptions;
use crate::module_loader::ModuleLoadPreparer;
use crate::module_loader::PrepareModuleLoadOptions;
use crate::util::fs::canonicalize_path;
use crate::util::fs::collect_specifiers;
use crate::util::path::is_script_ext;

pub trait ModuleGraphContainer: Clone + 'static {
  /// Acquires a permit to modify the module graph without other code
  /// having the chance to modify it. In the meantime, other code may
  /// still read from the existing module graph.
  async fn acquire_update_permit(&self) -> impl ModuleGraphUpdatePermit;
  /// Gets a copy of the graph.
  fn graph(&self) -> Arc<ModuleGraph>;
}

/// A permit for updating the module graph. When complete and
/// everything looks fine, calling `.commit()` will store the
/// new graph in the ModuleGraphContainer.
pub trait ModuleGraphUpdatePermit {
  /// Gets the module graph for mutation.
  fn graph_mut(&mut self) -> &mut ModuleGraph;
  /// Saves the mutated module graph in the container.
  fn commit(self);
}

/// Holds the `ModuleGraph` for the main worker.
#[derive(Clone)]
pub struct MainModuleGraphContainer {
  // Allow only one request to update the graph data at a time,
  // but allow other requests to read from it at any time even
  // while another request is updating the data.
  update_queue: Arc<deno_core::unsync::sync::TaskQueue>,
  inner: Arc<RwLock<Arc<ModuleGraph>>>,
  cli_options: Arc<CliOptions>,
  module_load_preparer: Arc<ModuleLoadPreparer>,
  root_permissions: PermissionsContainer,
}

#[derive(Default, Debug)]
pub struct CheckSpecifiersOptions<'a> {
  pub ext_overwrite: Option<&'a String>,
  pub allow_unknown_media_types: bool,
}

pub enum HmrReload {
  Reloaded(Vec<ModuleSpecifier>),
  NotInGraph(PathBuf),
}

fn find_graph_specifier(
  graph: &ModuleGraph,
  path: &Path,
) -> Option<ModuleSpecifier> {
  if let Ok(specifier) = ModuleSpecifier::from_file_path(path)
    && graph.contains(&specifier)
  {
    return Some(specifier);
  }
  graph
    .specifiers()
    .map(|(specifier, _)| specifier)
    .filter(|specifier| specifier.scheme() == "file")
    .find(|specifier| {
      specifier
        .to_file_path()
        .ok()
        .and_then(|p| canonicalize_path(&p).ok())
        .is_some_and(|p| p == path)
    })
    .cloned()
}

pub struct CollectSpecifiersOptions {
  /// Whether to include paths that are specified even if they're ignored.
  pub include_ignored_specified: bool,
}

impl MainModuleGraphContainer {
  pub fn new(
    cli_options: Arc<CliOptions>,
    module_load_preparer: Arc<ModuleLoadPreparer>,
    root_permissions: PermissionsContainer,
  ) -> Self {
    Self {
      update_queue: Default::default(),
      inner: Arc::new(RwLock::new(Arc::new(ModuleGraph::new(
        cli_options.graph_kind(),
      )))),
      cli_options,
      module_load_preparer,
      root_permissions,
    }
  }

  pub async fn check_specifiers(
    &self,
    specifiers: &[ModuleSpecifier],
    options: CheckSpecifiersOptions<'_>,
  ) -> Result<(), AnyError> {
    let mut graph_permit = self.acquire_update_permit().await;
    let graph = graph_permit.graph_mut();
    self
      .module_load_preparer
      .prepare_module_load(
        graph,
        specifiers,
        PrepareModuleLoadOptions {
          is_dynamic: false,
          lib: self.cli_options.ts_type_lib_window(),
          permissions: self.root_permissions.clone(),
          file_permission_api_name: None,
          ext_overwrite: options.ext_overwrite,
          allow_unknown_media_types: options.allow_unknown_media_types,
          allow_sloppy_imports_hints_for_unreferenced_roots: true,
          skip_graph_roots_validation: false,
          file_content_overrides: Default::default(),
          file_header_overrides: Default::default(),
        },
      )
      .await?;
    graph_permit.commit();
    Ok(())
  }

  pub async fn hmr_reload_paths(
    &self,
    paths: &[PathBuf],
  ) -> Result<HmrReload, AnyError> {
    let mut graph_permit = self.acquire_update_permit().await;
    let graph = graph_permit.graph_mut();
    let mut specifiers = Vec::with_capacity(paths.len());
    let mut specifiers_to_reload = Vec::with_capacity(paths.len());
    for path in paths {
      let Some(specifier) = find_graph_specifier(graph, path) else {
        return Ok(HmrReload::NotInGraph(path.clone()));
      };
      let is_unchanged = match graph.get(&specifier) {
        Some(Module::Js(module)) => std::fs::read_to_string(path)
          .is_ok_and(|text| *text == *module.source.text),
        _ => false,
      };
      if !is_unchanged {
        specifiers_to_reload.push(specifier.clone());
      }
      specifiers.push(specifier);
    }
    if !specifiers_to_reload.is_empty() {
      self
        .module_load_preparer
        .reload_specifiers(
          graph,
          specifiers_to_reload,
          false,
          self.root_permissions.clone(),
        )
        .await?;
      self.module_load_preparer.graph_roots_valid(
        graph,
        &graph.roots.iter().cloned().collect::<Vec<_>>(),
        GraphRootsValidOptions {
          allow_unknown_media_types: true,
          allow_unknown_jsr_exports: false,
          allow_sloppy_imports_hints_for_unreferenced_roots: true,
        },
      )?;
      graph_permit.commit();
    }
    Ok(HmrReload::Reloaded(specifiers))
  }

  pub fn collect_specifiers(
    &self,
    files: &[String],
    options: CollectSpecifiersOptions,
  ) -> Result<Vec<ModuleSpecifier>, AnyError> {
    let excludes = self.cli_options.workspace().resolve_config_excludes()?;
    let include_patterns =
      PathOrPatternSet::from_include_relative_path_or_patterns(
        self.cli_options.initial_cwd(),
        files,
      )?;
    let file_patterns = FilePatterns {
      base: self.cli_options.initial_cwd().to_path_buf(),
      include: Some(include_patterns),
      exclude: excludes,
    };
    collect_specifiers(
      crate::util::fs::CollectSpecifiersOptions {
        file_patterns,
        vendor_folder: self
          .cli_options
          .vendor_dir_path()
          .map(ToOwned::to_owned),
        include_ignored_specified: options.include_ignored_specified,
      },
      |e| is_script_ext(e.path),
    )
  }
}

impl ModuleGraphContainer for MainModuleGraphContainer {
  async fn acquire_update_permit(&self) -> impl ModuleGraphUpdatePermit {
    let permit = self.update_queue.acquire().await;
    MainModuleGraphUpdatePermit {
      permit,
      inner: self.inner.clone(),
      graph: (**self.inner.read()).clone(),
    }
  }

  fn graph(&self) -> Arc<ModuleGraph> {
    self.inner.read().clone()
  }
}

/// A permit for updating the module graph. When complete and
/// everything looks fine, calling `.commit()` will store the
/// new graph in the ModuleGraphContainer.
pub struct MainModuleGraphUpdatePermit<'a> {
  permit: deno_core::unsync::sync::TaskQueuePermit<'a>,
  inner: Arc<RwLock<Arc<ModuleGraph>>>,
  graph: ModuleGraph,
}

impl ModuleGraphUpdatePermit for MainModuleGraphUpdatePermit<'_> {
  fn graph_mut(&mut self) -> &mut ModuleGraph {
    &mut self.graph
  }

  fn commit(self) {
    *self.inner.write() = Arc::new(self.graph);
    drop(self.permit); // explicit drop for clarity
  }
}
