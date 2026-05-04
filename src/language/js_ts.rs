use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use tree_sitter::{Language, Node, Parser, Tree};
use tree_sitter_highlight::HighlightConfiguration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsTsDialect {
    JavaScript,
    Jsx,
    TypeScript,
    Tsx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsTsScarStyle {
    LineComment,
    JsxBlockComment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct JsTsScarPlacement {
    pub insert_before_line_1indexed: usize,
    pub style: JsTsScarStyle,
}

pub(crate) fn dialect_for_path(path: &Path) -> Option<JsTsDialect> {
    let ext = path.extension().and_then(|e| e.to_str())?;
    match ext.to_ascii_lowercase().as_str() {
        "js" | "mjs" | "cjs" => Some(JsTsDialect::JavaScript),
        "jsx" => Some(JsTsDialect::Jsx),
        "ts" | "mts" | "cts" => Some(JsTsDialect::TypeScript),
        "tsx" => Some(JsTsDialect::Tsx),
        _ => None,
    }
}

pub(crate) fn scar_placement_for_line(
    dialect: JsTsDialect,
    source: &str,
    target_line_1indexed: usize,
) -> Result<JsTsScarPlacement> {
    let target = target_line_1indexed.max(1);
    if matches!(dialect, JsTsDialect::JavaScript | JsTsDialect::TypeScript) {
        return Ok(JsTsScarPlacement {
            insert_before_line_1indexed: target,
            style: JsTsScarStyle::LineComment,
        });
    }

    let tree = parse_source(dialect, source)?;
    let root = tree.root_node();
    if root.has_error() {
        bail!("could not safely place JSX/TSX scar: parse tree contains errors");
    }

    let row = target - 1;
    let lines = source.lines().collect::<Vec<_>>();
    if let Some(opening) = deepest_node_containing_row(
        root,
        row,
        &["jsx_opening_element", "jsx_self_closing_element"],
    ) {
        let opening_start_row = opening.start_position().row;
        if line_starts_like_jsx(lines.get(opening_start_row).copied().unwrap_or_default()) {
            return Ok(JsTsScarPlacement {
                insert_before_line_1indexed: jsx_container_start_line(opening)
                    .unwrap_or(opening_start_row + 1),
                style: JsTsScarStyle::JsxBlockComment,
            });
        }
    }

    if let Some(container) =
        deepest_node_containing_row(root, row, &["jsx_element", "jsx_fragment"])
    {
        let current_line = lines.get(row).copied().unwrap_or_default();
        let container_start = container.start_position().row;
        let container_start_line = lines.get(container_start).copied().unwrap_or_default();
        let inside_container_body = row > container_start && row < container.end_position().row;
        if line_starts_like_jsx(current_line)
            || current_line.trim_start().starts_with('{')
            || (inside_container_body && line_starts_like_jsx(container_start_line))
        {
            return Ok(JsTsScarPlacement {
                insert_before_line_1indexed: target,
                style: JsTsScarStyle::JsxBlockComment,
            });
        }
    }

    Ok(JsTsScarPlacement {
        insert_before_line_1indexed: target,
        style: JsTsScarStyle::LineComment,
    })
}

pub(crate) fn parse_source(dialect: JsTsDialect, source: &str) -> Result<Tree> {
    let mut parser = Parser::new();
    let language = language_for_dialect(dialect);
    parser
        .set_language(&language)
        .with_context(|| format!("loading tree-sitter language for {dialect:?}"))?;
    parser
        .parse(source, None)
        .ok_or_else(|| anyhow!("tree-sitter returned no parse tree for {dialect:?}"))
}

pub(crate) fn highlight_configuration(
    dialect: JsTsDialect,
) -> Result<HighlightConfiguration, tree_sitter::QueryError> {
    let language = language_for_dialect(dialect);
    let (name, highlights, injections, locals) = match dialect {
        JsTsDialect::JavaScript => (
            "javascript",
            tree_sitter_javascript::HIGHLIGHT_QUERY.to_string(),
            tree_sitter_javascript::INJECTIONS_QUERY,
            tree_sitter_javascript::LOCALS_QUERY,
        ),
        JsTsDialect::Jsx => (
            "jsx",
            format!(
                "{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
            ),
            tree_sitter_javascript::INJECTIONS_QUERY,
            tree_sitter_javascript::LOCALS_QUERY,
        ),
        JsTsDialect::TypeScript => (
            "typescript",
            tree_sitter_typescript::HIGHLIGHTS_QUERY.to_string(),
            "",
            tree_sitter_typescript::LOCALS_QUERY,
        ),
        JsTsDialect::Tsx => (
            "tsx",
            format!(
                "{}\n{}",
                tree_sitter_typescript::HIGHLIGHTS_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
            ),
            "",
            tree_sitter_typescript::LOCALS_QUERY,
        ),
    };
    HighlightConfiguration::new(language, name, &highlights, injections, locals)
}

fn language_for_dialect(dialect: JsTsDialect) -> Language {
    match dialect {
        JsTsDialect::JavaScript | JsTsDialect::Jsx => tree_sitter_javascript::LANGUAGE.into(),
        JsTsDialect::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        JsTsDialect::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
    }
}

fn deepest_node_containing_row<'tree>(
    node: Node<'tree>,
    row: usize,
    kinds: &[&str],
) -> Option<Node<'tree>> {
    if !node_contains_row(node, row) {
        return None;
    }
    let mut cursor = node.walk();
    let mut deepest = None;
    for child in node.children(&mut cursor) {
        if let Some(found) = deepest_node_containing_row(child, row, kinds) {
            deepest = Some(found);
        }
    }
    if deepest.is_some() {
        deepest
    } else {
        kinds.contains(&node.kind()).then_some(node)
    }
}

fn node_contains_row(node: Node<'_>, row: usize) -> bool {
    node.start_position().row <= row && row <= node.end_position().row
}

fn jsx_container_start_line(node: Node<'_>) -> Option<usize> {
    let mut current = Some(node);
    while let Some(n) = current {
        if matches!(
            n.kind(),
            "jsx_element" | "jsx_fragment" | "jsx_self_closing_element"
        ) {
            return Some(n.start_position().row + 1);
        }
        current = n.parent();
    }
    None
}

fn line_starts_like_jsx(line: &str) -> bool {
    line.trim_start().starts_with('<')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tsx_parser_accepts_react_component_fixture() {
        let source = r#"
type CounterProps = { count: number };

export function Counter({ count }: CounterProps) {
  return (
    <section className="counter">
      <p>Count: {count}</p>
    </section>
  );
}
"#;

        let tree = parse_source(JsTsDialect::Tsx, source).expect("tsx parses");
        assert!(
            !tree.root_node().has_error(),
            "TSX fixture should not contain parse errors: {:#?}",
            tree.root_node()
        );
    }

    #[test]
    fn tsx_highlight_configuration_builds() {
        let config =
            highlight_configuration(JsTsDialect::Tsx).expect("tsx highlight config builds");
        assert!(
            config.names().contains(&"tag"),
            "combined TSX query should include JSX tag captures"
        );
    }

    #[test]
    fn scar_placement_relocates_attribute_rows_to_jsx_element_start() {
        let source = "export function Panel() {\n  return (\n    <Button\n      kind=\"primary\"\n    >\n      Save\n    </Button>\n  );\n}\n";

        let placement = scar_placement_for_line(JsTsDialect::Tsx, source, 4).expect("placement");

        assert_eq!(
            placement,
            JsTsScarPlacement {
                insert_before_line_1indexed: 3,
                style: JsTsScarStyle::JsxBlockComment,
            }
        );
    }
}
