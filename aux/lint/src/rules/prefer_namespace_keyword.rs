// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use deno_ast::SourceRanged;
use deno_ast::view::TsModuleDecl;
use deno_ast::view::TsModuleName;
use once_cell::sync::Lazy;
use regex::Regex;

use super::Context;
use super::LintRule;
use crate::Program;
use crate::handler::Handler;
use crate::handler::Traverse;
use crate::tags::Tags;
use crate::tags::{self};

#[derive(Debug)]
pub struct PreferNamespaceKeyword;

const CODE: &str = "prefer-namespace-keyword";
const MESSAGE: &str = "`module` keyword in module declaration is not allowed";

impl LintRule for PreferNamespaceKeyword {
  fn tags(&self) -> Tags {
    &[tags::RECOMMENDED]
  }

  fn code(&self) -> &'static str {
    CODE
  }

  fn lint_program_with_ast_view(
    &self,
    context: &mut Context,
    program: Program,
  ) {
    PreferNamespaceKeywordHandler.traverse(program, context);
  }
}

struct PreferNamespaceKeywordHandler;

impl Handler for PreferNamespaceKeywordHandler {
  fn ts_module_decl(&mut self, mod_decl: &TsModuleDecl, ctx: &mut Context) {
    if let TsModuleName::Str(_) = &mod_decl.id {
      return;
    }
    static KEYWORD: Lazy<Regex> =
      Lazy::new(|| Regex::new(r"(declare\s)?(?P<keyword>\w+)").unwrap());

    let snippet = mod_decl.text_fast(ctx.text_info());
    if let Some(capt) = KEYWORD.captures(snippet) {
      let keyword = capt.name("keyword").unwrap().as_str();
      if keyword == "module" && !mod_decl.global() {
        ctx.add_diagnostic(mod_decl.range(), CODE, MESSAGE)
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn prefer_namespace_keyword_valid() {
    assert_lint_ok! {
      PreferNamespaceKeyword,
      "declare module 'foo';",
      "declare module 'foo' {}",
      "namespace foo {}",
      "declare namespace foo {}",
      "declare global {}",
    };
  }

  #[test]
  fn prefer_namespace_keyword_invalid() {
    assert_lint_err! {
      PreferNamespaceKeyword,
      r#"module foo {}"#: [{ col: 0, message: MESSAGE }],
      r#"
      declare module foo {
        declare module bar {}
      }"#: [{ line: 2, col: 6, message: MESSAGE}, { line: 3, col: 8, message: MESSAGE }],
    }
  }
}
