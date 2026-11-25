// Copyright 2018-2026 the Deno authors. MIT license.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use deno_error::JsErrorBox;

use crate::HmrUpdateOutcome;
use crate::ModuleSource;
use crate::ModuleSpecifier;
use crate::ModuleType;
use crate::ResolutionKind;
use crate::RuntimeOptions;
use crate::modules::ModuleLoadOptions;
use crate::modules::ModuleLoadReferrer;
use crate::modules::ModuleLoadResponse;
use crate::modules::ModuleLoader;
use crate::modules::ModuleResolveResponse;
use crate::modules::ModuleSourceCode;
use crate::resolve_import;
use crate::runtime::JsRuntime;

type Files = Rc<RefCell<HashMap<String, String>>>;

struct HmrLoader(Files);

impl ModuleLoader for HmrLoader {
  fn resolve(
    &self,
    specifier: &str,
    referrer: &str,
    _kind: ResolutionKind,
  ) -> ModuleResolveResponse {
    resolve_import(specifier, referrer).map_err(JsErrorBox::from_err)
  }

  fn load(
    &self,
    module_specifier: &ModuleSpecifier,
    _maybe_referrer: Option<&ModuleLoadReferrer>,
    _options: ModuleLoadOptions,
  ) -> ModuleLoadResponse {
    let res = match self.0.borrow().get(module_specifier.as_str()) {
      Some(code) => Ok(ModuleSource::new(
        ModuleType::JavaScript,
        ModuleSourceCode::String(code.clone().into()),
        module_specifier,
        None,
      )),
      None => Err(JsErrorBox::generic("Module not found")),
    };
    ModuleLoadResponse::Sync(res)
  }
}

struct Harness {
  runtime: JsRuntime,
  files: Files,
}

impl Harness {
  async fn new(files: &[(&str, &str)], main: &str) -> Self {
    let files: Files = Rc::new(RefCell::new(
      files
        .iter()
        .map(|(name, code)| (format!("file:///{name}"), code.to_string()))
        .collect(),
    ));
    let mut runtime = JsRuntime::new(RuntimeOptions {
      module_loader: Some(Rc::new(HmrLoader(files.clone()))),
      ..Default::default()
    });
    runtime.enable_hmr();
    runtime
      .execute_script("setup.js", "globalThis.log = [];")
      .unwrap();
    let main = ModuleSpecifier::parse(&format!("file:///{main}")).unwrap();
    let id = runtime.load_main_es_module(&main).await.unwrap();
    let result = runtime.mod_evaluate(id);
    runtime.run_event_loop(Default::default()).await.unwrap();
    result.await.unwrap();
    Self { runtime, files }
  }

  fn write(&self, name: &str, code: &str) {
    self
      .files
      .borrow_mut()
      .insert(format!("file:///{name}"), code.to_string());
  }

  async fn update(&mut self, names: &[&str]) -> HmrUpdateOutcome {
    self.try_update(names).await.unwrap()
  }

  async fn try_update(
    &mut self,
    names: &[&str],
  ) -> Result<HmrUpdateOutcome, crate::error::CoreError> {
    self
      .runtime
      .hmr_apply_update(
        names.iter().map(|name| format!("file:///{name}")).collect(),
      )
      .await
  }

  fn log(&mut self) -> Vec<String> {
    let value = self
      .runtime
      .execute_script("log.js", "globalThis.log.splice(0)")
      .unwrap();
    crate::scope!(scope, &mut self.runtime);
    let value = crate::v8::Local::new(scope, value);
    serde_v8::from_v8(scope, value).unwrap()
  }
}

fn handled(names: &[&str]) -> HmrUpdateOutcome {
  handled_pruned(names, &[])
}

fn handled_pruned(names: &[&str], pruned: &[&str]) -> HmrUpdateOutcome {
  HmrUpdateOutcome::Handled {
    updated: names.iter().map(|name| format!("file:///{name}")).collect(),
    pruned: pruned
      .iter()
      .map(|name| format!("file:///{name}"))
      .collect(),
  }
}

fn is_full_reload(outcome: &HmrUpdateOutcome) -> bool {
  matches!(outcome, HmrUpdateOutcome::NeedsFullReload { .. })
}

#[tokio::test]
async fn hmr_disabled_has_no_hot() {
  let files: Files = Rc::new(RefCell::new(HashMap::from([(
    "file:///a.js".to_string(),
    "globalThis.hot = typeof import.meta.hot;".to_string(),
  )])));
  let mut runtime = JsRuntime::new(RuntimeOptions {
    module_loader: Some(Rc::new(HmrLoader(files))),
    ..Default::default()
  });
  let main = ModuleSpecifier::parse("file:///a.js").unwrap();
  let id = runtime.load_main_es_module(&main).await.unwrap();
  let result = runtime.mod_evaluate(id);
  runtime.run_event_loop(Default::default()).await.unwrap();
  result.await.unwrap();
  let value = runtime
    .execute_script("check.js", "globalThis.hot")
    .unwrap();
  crate::scope!(scope, &mut runtime);
  let value = crate::v8::Local::new(scope, value);
  assert_eq!(
    serde_v8::from_v8::<String>(scope, value).unwrap(),
    "undefined"
  );
}

#[tokio::test]
async fn hmr_linked_importer_keeps_old_record() {
  let mut h = Harness::new(
    &[
      (
        "a.js",
        "import { c } from './c.js'; globalThis.readC = () => c;",
      ),
      (
        "c.js",
        "export const c = 1; import.meta.hot.accept(({ module }) => log.push(`accepted ${module.c}`));",
      ),
    ],
    "a.js",
  )
  .await;
  h.write("c.js", "export const c = 2; import.meta.hot.accept();");
  assert_eq!(h.update(&["c.js"]).await, handled(&["c.js"]));
  assert_eq!(h.log(), vec!["accepted 2"]);
  h.runtime
    .execute_script("read.js", "log.push(String(readC()))")
    .unwrap();
  assert_eq!(h.log(), vec!["1"]);
}

#[tokio::test]
async fn hmr_no_boundary_needs_full_reload() {
  let mut h = Harness::new(
    &[("a.js", "import './b.js';"), ("b.js", "log.push('b1');")],
    "a.js",
  )
  .await;
  h.log();
  h.write("b.js", "log.push('b2');");
  assert!(is_full_reload(&h.update(&["b.js"]).await));
  assert!(is_full_reload(&h.update(&["missing.js"]).await));
  assert_eq!(h.log(), Vec::<String>::new());
}

#[tokio::test]
async fn hmr_three_level_chain_self_accept_in_middle() {
  let mut h = Harness::new(
    &[
      ("a.js", "import './b.js'; log.push('a');"),
      (
        "b.js",
        "import { c } from './c.js'; log.push(`b ${c}`); import.meta.hot.accept();",
      ),
      ("c.js", "export const c = 1;"),
    ],
    "a.js",
  )
  .await;
  assert_eq!(h.log(), vec!["b 1", "a"]);
  h.write("c.js", "export const c = 2;");
  assert_eq!(h.update(&["c.js"]).await, handled(&["c.js", "b.js"]));
  assert_eq!(h.log(), vec!["b 2"]);
  h.write("c.js", "export const c = 3;");
  assert_eq!(h.update(&["c.js"]).await, handled(&["c.js", "b.js"]));
  assert_eq!(h.log(), vec!["b 3"]);
}

#[tokio::test]
async fn hmr_three_level_chain_dep_accept_at_top() {
  let mut h = Harness::new(
    &[
      (
        "a.js",
        "import { b } from './b.js'; log.push(`a ${b}`);
         import.meta.hot.accept('./b.js', (dep) => log.push(`a got ${dep.b} ${dep.module === undefined} ${dep.deps[0].b}`));",
      ),
      (
        "b.js",
        "import { c } from './c.js'; export const b = `b${c}`; log.push('b');",
      ),
      ("c.js", "export const c = 1;"),
    ],
    "a.js",
  )
  .await;
  assert_eq!(h.log(), vec!["b", "a b1"]);
  h.write("c.js", "export const c = 2;");
  assert_eq!(h.update(&["c.js"]).await, handled(&["c.js", "b.js"]));
  assert_eq!(h.log(), vec!["b", "a got b2 false b2"]);
}

#[tokio::test]
async fn hmr_array_deps_payload() {
  let mut h = Harness::new(
    &[
      (
        "a.js",
        "import './b.js'; import './c.js';
         import.meta.hot.accept(['./b.js', './c.js'], (payload) => {
           const [b, c] = payload;
           const { module, deps } = payload;
           log.push(`${b.v} ${c.v} ${deps.length} ${typeof module}`);
         });",
      ),
      ("b.js", "export const v = 'b1';"),
      ("c.js", "export const v = 'c1';"),
    ],
    "a.js",
  )
  .await;
  h.write("b.js", "export const v = 'b2';");
  h.write("c.js", "export const v = 'c2';");
  assert_eq!(
    h.update(&["b.js", "c.js"]).await,
    handled(&["b.js", "c.js"])
  );
  assert_eq!(h.log(), vec!["b2 c2 2 object"]);
}

#[tokio::test]
async fn hmr_diamond() {
  let mut h = Harness::new(
    &[
      (
        "a.js",
        "import './b1.js'; import { v } from './b2.js';
         import.meta.hot.accept('./b2.js', ({ deps }) => log.push(`a got ${deps[0].v}`));",
      ),
      (
        "b1.js",
        "import { c } from './c.js'; log.push(`b1 ${c}`); import.meta.hot.accept();",
      ),
      (
        "b2.js",
        "import { c } from './c.js'; export const v = `b2 ${c}`; log.push(v);",
      ),
      ("c.js", "export const c = 1; log.push('c');"),
    ],
    "a.js",
  )
  .await;
  h.log();
  h.write("c.js", "export const c = 2; log.push('c');");
  assert_eq!(
    h.update(&["c.js"]).await,
    handled(&["c.js", "b1.js", "b2.js"])
  );
  assert_eq!(h.log(), vec!["c", "b1 2", "b2 2", "a got b2 2"]);
}

#[tokio::test]
async fn hmr_one_unaccepting_importer_needs_full_reload() {
  let mut h = Harness::new(
    &[
      ("a.js", "import './b1.js'; import './b2.js';"),
      ("b1.js", "import './c.js'; import.meta.hot.accept();"),
      ("b2.js", "import './c.js';"),
      ("c.js", "export const c = 1;"),
    ],
    "a.js",
  )
  .await;
  h.write("c.js", "export const c = 2;");
  assert!(is_full_reload(&h.update(&["c.js"]).await));
}

#[tokio::test]
async fn hmr_removed_and_added_import() {
  let mut h = Harness::new(
    &[
      ("a.js", "import './b.js';"),
      ("b.js", "import './c.js'; import.meta.hot.accept();"),
      ("c.js", "log.push('c');"),
      ("d.js", "log.push('d');"),
    ],
    "a.js",
  )
  .await;
  h.log();
  h.write("b.js", "import './d.js'; import.meta.hot.accept();");
  assert_eq!(
    h.update(&["b.js"]).await,
    handled_pruned(&["b.js"], &["c.js"])
  );
  assert_eq!(h.log(), vec!["d"]);
  h.write("c.js", "log.push('c2');");
  assert!(is_full_reload(&h.update(&["c.js"]).await));
  h.write("d.js", "log.push('d2');");
  assert_eq!(h.update(&["d.js"]).await, handled(&["d.js", "b.js"]));
  assert_eq!(h.log(), vec!["d2"]);
}

#[tokio::test]
async fn hmr_dispose_and_data_handoff() {
  let code = "const count = (import.meta.hot.data.count ?? 0) + 1;
    log.push(`run ${count}`);
    import.meta.hot.dispose((data) => { data.count = count; log.push(`dispose ${count}`); });
    import.meta.hot.accept();";
  let mut h = Harness::new(&[("a.js", code)], "a.js").await;
  h.write("a.js", &format!("{code} // 2"));
  assert_eq!(h.update(&["a.js"]).await, handled(&["a.js"]));
  h.write("a.js", &format!("{code} // 3"));
  assert_eq!(h.update(&["a.js"]).await, handled(&["a.js"]));
  assert_eq!(
    h.log(),
    vec!["run 1", "dispose 1", "run 2", "dispose 2", "run 3"]
  );
}

#[tokio::test]
async fn hmr_main_module_stays_main() {
  let code = "log.push(`main ${import.meta.main}`); import.meta.hot.accept();";
  let mut h = Harness::new(&[("a.js", code)], "a.js").await;
  h.write("a.js", &format!("{code} // 2"));
  assert_eq!(h.update(&["a.js"]).await, handled(&["a.js"]));
  assert_eq!(h.log(), vec!["main true", "main true"]);
}

#[tokio::test]
async fn hmr_decline() {
  let mut h = Harness::new(
    &[
      ("a.js", "import './b.js'; import.meta.hot.accept('./b.js');"),
      ("b.js", "import.meta.hot.decline();"),
    ],
    "a.js",
  )
  .await;
  h.write("b.js", "import.meta.hot.decline(); // 2");
  assert!(is_full_reload(&h.update(&["b.js"]).await));
}

#[tokio::test]
async fn hmr_invalidate_bubbles_to_importer() {
  let mut h = Harness::new(
    &[
      (
        "a.js",
        "import './b.js'; import.meta.hot.accept('./b.js', () => log.push('a accepted b'));",
      ),
      ("b.js", "export const ok = true; import.meta.hot.accept();"),
    ],
    "a.js",
  )
  .await;
  h.write(
    "b.js",
    "export const ok = false; log.push('b evaluated');
     import.meta.hot.accept(); import.meta.hot.invalidate();",
  );
  assert_eq!(h.update(&["b.js"]).await, handled(&["b.js"]));
  assert_eq!(h.log(), vec!["b evaluated", "b evaluated", "a accepted b"]);
}

#[tokio::test]
async fn hmr_invalidate_without_importer_needs_full_reload() {
  let mut h =
    Harness::new(&[("a.js", "import.meta.hot.accept();")], "a.js").await;
  h.write(
    "a.js",
    "import.meta.hot.accept(); import.meta.hot.invalidate();",
  );
  assert!(is_full_reload(&h.update(&["a.js"]).await));
}

#[tokio::test]
async fn hmr_evaluation_error() {
  let mut h = Harness::new(
    &[
      (
        "a.js",
        "import './b.js'; import './c.js';
         import.meta.hot.accept('./c.js', () => {}, (err) => log.push(`handled ${err.message}`));",
      ),
      ("b.js", "import.meta.hot.accept();"),
      ("c.js", ""),
    ],
    "a.js",
  )
  .await;
  h.write("b.js", "throw new Error('boom b');");
  assert!(h.try_update(&["b.js"]).await.is_err());
  h.write("c.js", "throw new Error('boom c');");
  assert_eq!(h.update(&["c.js"]).await, handled(&["c.js"]));
  assert_eq!(h.log(), vec!["handled boom c"]);
}

#[tokio::test]
async fn hmr_stale_instance_cannot_register() {
  let mut h = Harness::new(
    &[(
      "a.js",
      "globalThis.lateDecline = () => import.meta.hot.decline(); import.meta.hot.accept();",
    )],
    "a.js",
  )
  .await;
  h.write("a.js", "log.push('a2'); import.meta.hot.accept();");
  assert_eq!(h.update(&["a.js"]).await, handled(&["a.js"]));
  h.runtime
    .execute_script("late.js", "lateDecline()")
    .unwrap();
  h.write("a.js", "log.push('a3'); import.meta.hot.accept();");
  assert_eq!(h.update(&["a.js"]).await, handled(&["a.js"]));
  assert_eq!(h.log(), vec!["a2", "a3"]);
}

#[tokio::test]
async fn hmr_dynamic_import_edge() {
  let mut h = Harness::new(
    &[
      (
        "a.js",
        "const b = await import('./b.js'); log.push(b.v);
         import.meta.hot.accept('./b.js', (dep) => log.push(`a got ${dep.v}`));",
      ),
      ("b.js", "export const v = 'b1';"),
    ],
    "a.js",
  )
  .await;
  h.write("b.js", "export const v = 'b2';");
  assert_eq!(h.update(&["b.js"]).await, handled(&["b.js"]));
  assert_eq!(h.log(), vec!["b1", "a got b2"]);
}

#[tokio::test]
async fn hmr_prune_removed_import() {
  let mut h = Harness::new(
    &[
      (
        "a.js",
        "import './b.js'; import './keep.js'; import.meta.hot.accept();",
      ),
      (
        "b.js",
        "import './c.js'; import './keep.js';
         import.meta.hot.dispose(() => log.push('b dispose'));
         import.meta.hot.prune(() => log.push('b prune'));",
      ),
      ("c.js", "import.meta.hot.prune(() => log.push('c prune'));"),
      (
        "keep.js",
        "import.meta.hot.prune(() => log.push('keep prune'));",
      ),
    ],
    "a.js",
  )
  .await;
  h.write("a.js", "import './keep.js'; import.meta.hot.accept();");
  assert_eq!(
    h.update(&["a.js"]).await,
    handled_pruned(&["a.js"], &["b.js", "c.js"])
  );
  assert_eq!(h.log(), vec!["b dispose", "b prune", "c prune"]);
  assert!(is_full_reload(&h.update(&["b.js"]).await));

  h.write("a.js", "import './b.js'; import.meta.hot.accept();");
  h.write("b.js", "log.push('b again');");
  assert_eq!(
    h.update(&["a.js"]).await,
    handled_pruned(&["a.js"], &["keep.js"])
  );
  assert_eq!(h.log(), vec!["b again", "keep prune"]);
}
