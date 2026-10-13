// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.

use deno_ast::SourceRanged;
use deno_ast::view::JSXElement;
use deno_ast::view::JSXElementName;

use super::Context;
use super::LintRule;
use crate::Program;
use crate::handler::Handler;
use crate::handler::Traverse;
use crate::tags::Tags;
use crate::tags::{self};

#[derive(Debug)]
pub struct JSXVoidDomElementsNoChildren;

const CODE: &str = "jsx-void-dom-elements-no-children";

impl LintRule for JSXVoidDomElementsNoChildren {
  fn tags(&self) -> Tags {
    &[tags::RECOMMENDED, tags::REACT, tags::JSX, tags::FRESH]
  }

  fn code(&self) -> &'static str {
    CODE
  }

  fn lint_program_with_ast_view(
    &self,
    context: &mut Context,
    program: Program,
  ) {
    JSXVoidDomElementsNoChildrenHandler.traverse(program, context);
  }
}

const MESSAGE: &str = "Found one or more unescaped entities in JSX text";
const HINT: &str = "Escape the '\">} characters respectively";

struct JSXVoidDomElementsNoChildrenHandler;

impl Handler for JSXVoidDomElementsNoChildrenHandler {
  fn jsx_element(&mut self, node: &JSXElement, ctx: &mut Context) {
    if let JSXElementName::Ident(name) = node.opening.name {
      if !node.children.is_empty()
        && matches!(
          name.sym().as_str(),
          "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
        )
      {
        ctx.add_diagnostic_with_hint(node.range(), CODE, MESSAGE, HINT);
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn jsx_void_dom_elements_no_children_valid() {
    assert_lint_ok! {
      JSXVoidDomElementsNoChildren,
      filename: "file:///foo.jsx",
      r#"<br />"#,
      r#"<img />"#,
    };
  }

  #[test]
  fn jsx_void_dom_elements_no_children_invalid() {
    assert_lint_err! {
      JSXVoidDomElementsNoChildren,
      filename: "file:///foo.jsx",
      r#"<area>foo</area>"#: [
        {
          col: 0,
          message: MESSAGE,
          hint: HINT,
        }
      ],
      r#"<base>foo</base>"#: [
        {
          col: 0,
          message: MESSAGE,
          hint: HINT,
        }
      ],
      r#"<br>foo</br>"#: [
        {
          col: 0,
          message: MESSAGE,
          hint: HINT,
        }
      ],
      r#"<col>foo</col>"#: [
        {
          col: 0,
          message: MESSAGE,
          hint: HINT,
        }
      ],
      r#"<embed>foo</embed>"#: [
        {
          col: 0,
          message: MESSAGE,
          hint: HINT,
        }
      ],
      r#"<hr>foo</hr>"#: [
        {
          col: 0,
          message: MESSAGE,
          hint: HINT,
        }
      ],
      r#"<img>foo</img>"#: [
        {
          col: 0,
          message: MESSAGE,
          hint: HINT,
        }
      ],
      r#"<input>foo</input>"#: [
        {
          col: 0,
          message: MESSAGE,
          hint: HINT,
        }
      ],
      r#"<link>foo</link>"#: [
        {
          col: 0,
          message: MESSAGE,
          hint: HINT,
        }
      ],
      r#"<meta>foo</meta>"#: [
        {
          col: 0,
          message: MESSAGE,
          hint: HINT,
        }
      ],
      r#"<param>foo</param>"#: [
        {
          col: 0,
          message: MESSAGE,
          hint: HINT,
        }
      ],
      r#"<source>foo</source>"#: [
        {
          col: 0,
          message: MESSAGE,
          hint: HINT,
        }
      ],
      r#"<track>foo</track>"#: [
        {
          col: 0,
          message: MESSAGE,
          hint: HINT,
        }
      ],
      r#"<wbr>foo</wbr>"#: [
        {
          col: 0,
          message: MESSAGE,
          hint: HINT,
        }
      ],
    };
  }
}
