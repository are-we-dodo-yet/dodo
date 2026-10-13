// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use deno_ast::SourceRanged;
use deno_ast::view::JSXAttrName;
use deno_ast::view::JSXAttrOrSpread;
use deno_ast::view::JSXElement;
use deno_ast::view::JSXElementChild;
use once_cell::sync::Lazy;

use super::Context;
use super::LintRule;
use crate::Program;
use crate::handler::Handler;
use crate::handler::Traverse;
use crate::tags::Tags;
use crate::tags::{self};

#[derive(Debug)]
pub struct ReactNoDangerWithChildren;

const CODE: &str = "react-no-danger-with-children";

impl LintRule for ReactNoDangerWithChildren {
  fn tags(&self) -> Tags {
    &[tags::REACT, tags::FRESH]
  }

  fn code(&self) -> &'static str {
    CODE
  }

  fn lint_program_with_ast_view(
    &self,
    context: &mut Context,
    program: Program,
  ) {
    JSXNoDangerWithChildrenHandler.traverse(program, context);
  }
}

const MESSAGE: &str =
  "Using JSX children together with 'dangerouslySetInnerHTML' is invalid";
const HINT: &str = "Remove the JSX children";

static IGNORE_TEXT: Lazy<regex::Regex> =
  Lazy::new(|| regex::Regex::new(r#"^\n\s+$"#).unwrap());

struct JSXNoDangerWithChildrenHandler;

impl Handler for JSXNoDangerWithChildrenHandler {
  fn jsx_element(&mut self, node: &JSXElement, ctx: &mut Context) {
    for attr in node.opening.attrs {
      if let JSXAttrOrSpread::JSXAttr(attr) = attr {
        if let JSXAttrName::Ident(id) = attr.name {
          if id.sym() == "dangerouslySetInnerHTML" {
            let filtered = node
              .children
              .iter()
              .filter(|child| {
                if let JSXElementChild::JSXText(text) = child {
                  if IGNORE_TEXT.is_match(text.value()) {
                    return false;
                  }
                }

                true
              })
              .collect::<Vec<_>>();

            if !filtered.is_empty() {
              ctx.add_diagnostic_with_hint(id.range(), CODE, MESSAGE, HINT);
            }
          }
        }
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn jsx_no_danger_with_children_valid() {
    assert_lint_ok! {
      ReactNoDangerWithChildren,
      filename: "file:///foo.jsx",
      r#"<div dangerouslySetInnerHTML={{ __html: "foo" }} />"#,
      r#"<div dangerouslySetInnerHTML={{ __html: "foo" }}></div>"#,
      r#"<div dangerouslySetInnerHTML={{ __html: "foo" }}>
      </div>"#,
    };
  }

  #[test]
  fn jsx_no_danger_with_children_invalid() {
    assert_lint_err! {
      ReactNoDangerWithChildren,
      filename: "file:///foo.jsx",
      r#"<div dangerouslySetInnerHTML={{ __html: "foo" }}>foo</div>"#: [
        {
          col: 5,
          message: MESSAGE,
          hint: HINT
        }
      ]
    };
  }
}
