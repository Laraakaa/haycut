use std::{io, path::Path};

use tree_sitter::{Language, Node, Parser};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SymbolKind {
    Class,
    Enum,
    Export,
    Function,
    Impl,
    Interface,
    Method,
    Struct,
    Type,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub kind: SymbolKind,
    pub name: String,
    pub start_line: usize,
    pub end_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolLanguage {
    Python,
    Rust,
    Tsx,
    TypeScript,
}

impl SymbolLanguage {
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()? {
            "py" => Some(Self::Python),
            "rs" => Some(Self::Rust),
            "ts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            _ => None,
        }
    }
}

pub fn parse_symbols(source: &str, language: SymbolLanguage) -> io::Result<Vec<Symbol>> {
    match language {
        SymbolLanguage::Python => parse_with_language(
            source,
            tree_sitter_python::LANGUAGE.into(),
            python_symbol_from_node,
            "Python",
        ),
        SymbolLanguage::Rust => parse_rust_symbols(source),
        SymbolLanguage::Tsx => parse_with_language(
            source,
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            typescript_symbol_from_node,
            "TSX",
        ),
        SymbolLanguage::TypeScript => parse_with_language(
            source,
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            typescript_symbol_from_node,
            "TypeScript",
        ),
    }
}

pub fn parse_rust_symbols(source: &str) -> io::Result<Vec<Symbol>> {
    parse_with_language(
        source,
        tree_sitter_rust::LANGUAGE.into(),
        rust_symbol_from_node,
        "Rust",
    )
}

fn parse_with_language(
    source: &str,
    language: Language,
    symbol_from_node: fn(Node<'_>, &str) -> Option<Symbol>,
    language_name: &str,
) -> io::Result<Vec<Symbol>> {
    let mut parser = Parser::new();
    parser.set_language(&language).map_err(io::Error::other)?;
    let tree = parser.parse(source, None).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to parse {language_name} source"),
        )
    })?;
    let mut symbols = Vec::new();

    collect_symbols(tree.root_node(), source, &mut symbols, symbol_from_node);

    Ok(symbols)
}

fn collect_symbols(
    node: Node<'_>,
    source: &str,
    symbols: &mut Vec<Symbol>,
    symbol_from_node: fn(Node<'_>, &str) -> Option<Symbol>,
) {
    if let Some(symbol) = symbol_from_node(node, source) {
        symbols.push(symbol);
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_symbols(child, source, symbols, symbol_from_node);
    }
}

fn rust_symbol_from_node(node: Node<'_>, source: &str) -> Option<Symbol> {
    let (kind, name) = match node.kind() {
        "function_item" => (SymbolKind::Function, named_symbol(node, source)?),
        "struct_item" => (SymbolKind::Struct, named_symbol(node, source)?),
        "enum_item" => (SymbolKind::Enum, named_symbol(node, source)?),
        "impl_item" => (SymbolKind::Impl, impl_name(node, source)),
        _ => return None,
    };

    Some(Symbol {
        kind,
        name,
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
    })
}

fn typescript_symbol_from_node(node: Node<'_>, source: &str) -> Option<Symbol> {
    let (kind, name) = match node.kind() {
        "function_declaration" | "generator_function_declaration" => {
            (SymbolKind::Function, named_symbol(node, source)?)
        }
        "class_declaration" => (SymbolKind::Class, named_symbol(node, source)?),
        "method_definition" => (SymbolKind::Method, named_symbol(node, source)?),
        "interface_declaration" => (SymbolKind::Interface, named_symbol(node, source)?),
        "type_alias_declaration" => (SymbolKind::Type, named_symbol(node, source)?),
        "export_statement" => (SymbolKind::Export, export_name(node, source)),
        "variable_declarator" if has_function_value(node) => {
            (SymbolKind::Function, named_symbol(node, source)?)
        }
        _ => return None,
    };

    Some(Symbol {
        kind,
        name,
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
    })
}

fn python_symbol_from_node(node: Node<'_>, source: &str) -> Option<Symbol> {
    let (kind, name) = match node.kind() {
        "class_definition" => (SymbolKind::Class, named_symbol(node, source)?),
        "function_definition" if is_python_method(node) => {
            (SymbolKind::Method, named_symbol(node, source)?)
        }
        "function_definition" => (SymbolKind::Function, named_symbol(node, source)?),
        _ => return None,
    };

    Some(Symbol {
        kind,
        name,
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
    })
}

/// Extract callee names within `[start_byte, end_byte)` of `source`, in
/// source order with duplicates removed. Walks `call_expression`/`call`
/// nodes (and method-call/scoped-call variants) for all languages; for Rust,
/// also scans `macro_invocation` token trees for `identifier(` pairs, since
/// tree-sitter-rust parses macro arguments as opaque tokens and a pure
/// `call_expression` walk would miss a call site like
/// `assert_eq!(total_for(10, 100), 900)`.
pub fn parse_calls(
    source: &str,
    language: SymbolLanguage,
    start_byte: usize,
    end_byte: usize,
) -> io::Result<Vec<String>> {
    let (ts_language, language_name) = match language {
        SymbolLanguage::Python => (tree_sitter_python::LANGUAGE.into(), "Python"),
        SymbolLanguage::Rust => (tree_sitter_rust::LANGUAGE.into(), "Rust"),
        SymbolLanguage::Tsx => (tree_sitter_typescript::LANGUAGE_TSX.into(), "TSX"),
        SymbolLanguage::TypeScript => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "TypeScript",
        ),
    };

    let mut parser = Parser::new();
    parser
        .set_language(&ts_language)
        .map_err(io::Error::other)?;
    let tree = parser.parse(source, None).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to parse {language_name} source"),
        )
    })?;

    let Some(node) = tree
        .root_node()
        .descendant_for_byte_range(start_byte, end_byte)
    else {
        return Ok(Vec::new());
    };

    let mut calls = Vec::new();
    collect_calls(node, source, language, &mut calls);

    let mut seen = std::collections::HashSet::new();
    calls.retain(|name| seen.insert(name.clone()));

    Ok(calls)
}

fn collect_calls(node: Node<'_>, source: &str, language: SymbolLanguage, calls: &mut Vec<String>) {
    if language == SymbolLanguage::Rust && node.kind() == "macro_invocation" {
        collect_macro_token_calls(node, source, calls);
    }

    if let Some(name) = call_name_for_node(node, source) {
        calls.push(name);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_calls(child, source, language, calls);
    }
}

fn call_name_for_node(node: Node<'_>, source: &str) -> Option<String> {
    let function = match node.kind() {
        "call_expression" | "call" => node.child_by_field_name("function")?,
        _ => return None,
    };

    call_name_from_function_node(function, source)
}

fn call_name_from_function_node(node: Node<'_>, source: &str) -> Option<String> {
    let named = match node.kind() {
        "identifier" => node,
        "field_expression" => node.child_by_field_name("field")?,
        "scoped_identifier" => node.child_by_field_name("name")?,
        "member_expression" => node.child_by_field_name("property")?,
        "attribute" => node.child_by_field_name("attribute")?,
        _ => return None,
    };

    named.utf8_text(source.as_bytes()).ok().map(str::to_string)
}

/// Scan a `macro_invocation`'s token tree for `identifier` nodes immediately
/// followed by a `token_tree` sibling starting with `(` — the shape
/// `total_for(10, 100)` takes when tree-sitter-rust parses macro arguments as
/// opaque tokens rather than a real `call_expression`.
fn collect_macro_token_calls(node: Node<'_>, source: &str, calls: &mut Vec<String>) {
    if node.kind() == "token_tree" {
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
        for pair in children.windows(2) {
            let [identifier, next] = pair else { continue };
            if identifier.kind() != "identifier" || next.kind() != "token_tree" {
                continue;
            }
            if let Ok(name) = identifier.utf8_text(source.as_bytes()) {
                calls.push(name.to_string());
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_macro_token_calls(child, source, calls);
    }
}

fn named_symbol(node: Node<'_>, source: &str) -> Option<String> {
    node.child_by_field_name("name")
        .and_then(|name| name.utf8_text(source.as_bytes()).ok())
        .map(str::to_string)
}

fn has_function_value(node: Node<'_>) -> bool {
    node.child_by_field_name("value")
        .map(|value| matches!(value.kind(), "arrow_function" | "function_expression"))
        .unwrap_or(false)
}

fn is_python_method(node: Node<'_>) -> bool {
    node.parent()
        .and_then(|block| block.parent())
        .map(|parent| parent.kind() == "class_definition")
        .unwrap_or(false)
}

fn export_name(node: Node<'_>, source: &str) -> String {
    let text = node.utf8_text(source.as_bytes()).unwrap_or("export");

    text.lines().next().unwrap_or("export").trim().to_string()
}

fn impl_name(node: Node<'_>, source: &str) -> String {
    let text = node.utf8_text(source.as_bytes()).unwrap_or("impl");
    let header = text.split('{').next().unwrap_or("impl").trim();

    header.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rust_functions_structs_enums_and_impls() {
        let source = r#"pub struct Session {
    id: String,
}

enum SessionState {
    Valid,
    Expired,
}

impl Session {
    pub fn validate_session(&self) -> bool {
        true
    }
}

fn build_session() -> Session {
    Session { id: String::new() }
}
"#;

        let symbols = parse_rust_symbols(source).expect("Rust symbols should parse");

        assert!(symbols.iter().any(|symbol| {
            symbol.kind == SymbolKind::Struct
                && symbol.name == "Session"
                && symbol.start_line == 1
                && symbol.end_line == 3
        }));
        assert!(symbols.iter().any(|symbol| {
            symbol.kind == SymbolKind::Enum
                && symbol.name == "SessionState"
                && symbol.start_line == 5
                && symbol.end_line == 8
        }));
        assert!(symbols.iter().any(|symbol| {
            symbol.kind == SymbolKind::Impl
                && symbol.name == "impl Session"
                && symbol.start_line == 10
                && symbol.end_line == 14
        }));
        assert!(symbols.iter().any(|symbol| {
            symbol.kind == SymbolKind::Function
                && symbol.name == "validate_session"
                && symbol.start_line == 11
                && symbol.end_line == 13
        }));
        assert!(symbols.iter().any(|symbol| {
            symbol.kind == SymbolKind::Function
                && symbol.name == "build_session"
                && symbol.start_line == 16
                && symbol.end_line == 18
        }));
    }

    #[test]
    fn extracts_trait_impl_blocks_roughly() {
        let source = r#"impl Display for Session {
    fn fmt(&self) {}
}
"#;

        let symbols = parse_rust_symbols(source).expect("Rust symbols should parse");
        let impl_symbol = symbols
            .iter()
            .find(|symbol| symbol.kind == SymbolKind::Impl)
            .expect("impl symbol should exist");

        assert_eq!(impl_symbol.name, "impl Display for Session");
        assert_eq!(impl_symbol.start_line, 1);
        assert_eq!(impl_symbol.end_line, 3);
    }

    #[test]
    fn extracts_typescript_symbols() {
        let source = r#"export interface SessionLike {
    refresh(): void;
}

export type SessionId = string;

export class Session {
    refresh(): void {}
}

export function buildSession(): Session {
    return new Session();
}

export const createSession = () => new Session();
"#;

        let symbols = parse_symbols(source, SymbolLanguage::TypeScript)
            .expect("TypeScript symbols should parse");

        assert!(symbols.iter().any(|symbol| {
            symbol.kind == SymbolKind::Interface
                && symbol.name == "SessionLike"
                && symbol.start_line == 1
                && symbol.end_line == 3
        }));
        assert!(symbols.iter().any(|symbol| {
            symbol.kind == SymbolKind::Type
                && symbol.name == "SessionId"
                && symbol.start_line == 5
                && symbol.end_line == 5
        }));
        assert!(symbols.iter().any(|symbol| {
            symbol.kind == SymbolKind::Class
                && symbol.name == "Session"
                && symbol.start_line == 7
                && symbol.end_line == 9
        }));
        assert!(symbols.iter().any(|symbol| {
            symbol.kind == SymbolKind::Method
                && symbol.name == "refresh"
                && symbol.start_line == 8
                && symbol.end_line == 8
        }));
        assert!(symbols.iter().any(|symbol| {
            symbol.kind == SymbolKind::Function
                && symbol.name == "buildSession"
                && symbol.start_line == 11
                && symbol.end_line == 13
        }));
        assert!(symbols.iter().any(|symbol| {
            symbol.kind == SymbolKind::Function
                && symbol.name == "createSession"
                && symbol.start_line == 15
                && symbol.end_line == 15
        }));
        assert!(
            symbols.iter().any(
                |symbol| symbol.kind == SymbolKind::Export && symbol.name.starts_with("export")
            )
        );
    }

    #[test]
    fn extracts_python_symbols() {
        let source = r#"class Session:
    def refresh(self):
        return True


def build_session():
    return Session()
"#;

        let symbols =
            parse_symbols(source, SymbolLanguage::Python).expect("Python symbols should parse");

        assert!(symbols.iter().any(|symbol| {
            symbol.kind == SymbolKind::Class
                && symbol.name == "Session"
                && symbol.start_line == 1
                && symbol.end_line == 3
        }));
        assert!(symbols.iter().any(|symbol| {
            symbol.kind == SymbolKind::Method
                && symbol.name == "refresh"
                && symbol.start_line == 2
                && symbol.end_line == 3
        }));
        assert!(symbols.iter().any(|symbol| {
            symbol.kind == SymbolKind::Function
                && symbol.name == "build_session"
                && symbol.start_line == 6
                && symbol.end_line == 7
        }));
    }

    #[test]
    fn detects_symbol_language_from_path() {
        assert_eq!(
            SymbolLanguage::from_path(Path::new("src/session.ts")),
            Some(SymbolLanguage::TypeScript)
        );
        assert_eq!(
            SymbolLanguage::from_path(Path::new("src/session.tsx")),
            Some(SymbolLanguage::Tsx)
        );
        assert_eq!(
            SymbolLanguage::from_path(Path::new("src/session.py")),
            Some(SymbolLanguage::Python)
        );
        assert_eq!(SymbolLanguage::from_path(Path::new("README.md")), None);
    }

    #[test]
    fn parse_calls_finds_direct_method_and_scoped_rust_calls() {
        let source = r#"fn run() {
    helper();
    self.method_call();
    module::scoped_call();
}
"#;

        let calls = parse_calls(source, SymbolLanguage::Rust, 0, source.len())
            .expect("Rust calls should parse");

        assert!(calls.contains(&"helper".to_string()));
        assert!(calls.contains(&"method_call".to_string()));
        assert!(calls.contains(&"scoped_call".to_string()));
    }

    #[test]
    fn parse_calls_finds_calls_inside_assert_macro() {
        let source = r#"fn ten_units_qualifies_for_bulk_discount() {
    assert_eq!(total_for(10, 100), 900);
}
"#;

        let calls = parse_calls(source, SymbolLanguage::Rust, 0, source.len())
            .expect("Rust calls should parse");

        assert!(
            calls.contains(&"total_for".to_string()),
            "expected macro-token fallback to find `total_for`, got {calls:?}"
        );
    }

    #[test]
    fn parse_calls_finds_typescript_calls() {
        let source = r#"function run() {
    helper();
    obj.method();
}
"#;

        let calls = parse_calls(source, SymbolLanguage::TypeScript, 0, source.len())
            .expect("TypeScript calls should parse");

        assert!(calls.contains(&"helper".to_string()));
        assert!(calls.contains(&"method".to_string()));
    }

    #[test]
    fn parse_calls_finds_python_calls() {
        let source = "def run():\n    helper()\n    obj.method()\n";

        let calls = parse_calls(source, SymbolLanguage::Python, 0, source.len())
            .expect("Python calls should parse");

        assert!(calls.contains(&"helper".to_string()));
        assert!(calls.contains(&"method".to_string()));
    }
}
