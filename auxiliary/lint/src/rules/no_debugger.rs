// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use deno_ast::SourceRanged;
use deno_ast::view::DebuggerStmt;
use derive_more::Display;

use super::Context;
use super::LintRule;
use crate::Program;
use crate::handler::Handler;
use crate::handler::Traverse;
use crate::tags::Tags;
use crate::tags::{self};

#[derive(Debug)]
pub struct NoDebugger;

const CODE: &str = "no-debugger";

#[derive(Display)]
enum NoDebuggerMessage {
  #[display(fmt = "`debugger` statement is not allowed")]
  Unexpected,
}

#[derive(Display)]
enum NoDebuggerHint {
  #[display(fmt = "Remove the `debugger` statement")]
  Remove,
}

impl LintRule for NoDebugger {
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
    NoDebuggerHandler.traverse(program, context);
  }
}

struct NoDebuggerHandler;

impl Handler for NoDebuggerHandler {
  fn debugger_stmt(&mut self, debugger_stmt: &DebuggerStmt, ctx: &mut Context) {
    ctx.add_diagnostic_with_hint(
      debugger_stmt.range(),
      CODE,
      NoDebuggerMessage::Unexpected,
      NoDebuggerHint::Remove,
    );
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn no_debugger_invalid() {
    assert_lint_err! {
      NoDebugger,
      r#"function asdf(): number { console.log("asdf"); debugger; return 1; }"#: [
        {
          col: 47,
          message: NoDebuggerMessage::Unexpected,
          hint: NoDebuggerHint::Remove,
        }
      ]
    };
  }
}
