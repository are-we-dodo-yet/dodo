// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use deno_ast::SourceRanged;
use deno_ast::view as ast_view;

use super::Context;
use super::LintRule;
use crate::Program;
use crate::handler::Handler;
use crate::handler::Traverse;
use crate::tags::Tags;
use crate::tags::{self};

#[derive(Debug)]
pub struct NoWith;

const CODE: &str = "no-with";
const MESSAGE: &str = "`with` statement is not allowed";

impl LintRule for NoWith {
  fn tags(&self) -> Tags {
    &[tags::RECOMMENDED]
  }

  fn code(&self) -> &'static str {
    CODE
  }

  fn lint_program_with_ast_view(
    &self,
    context: &mut Context,
    program: Program<'_>,
  ) {
    NoWithHandler.traverse(program, context);
  }
}

struct NoWithHandler;

impl Handler for NoWithHandler {
  fn with_stmt(&mut self, with_stmt: &ast_view::WithStmt, ctx: &mut Context) {
    ctx.add_diagnostic(with_stmt.range(), CODE, MESSAGE);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn no_with_invalid() {
    assert_lint_err! {
      NoWith,
      "with (someVar) { console.log('asdf'); }": [{ col: 0, message: MESSAGE }],
    }
  }
}
