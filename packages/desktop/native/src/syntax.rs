//! Single-pass syntax highlighting for fenced code and tool output.
//!
//! The desktop reference uses CodeMirror grammars. Native rendering cannot run those, so this
//! lexer recognizes the same lexical classes (comments, strings, numbers, keywords, literals,
//! definitions, tags) with per-language rules. It is a lexer, not a parser: a construct it does
//! not know stays in the default color instead of being guessed.
use crate::theme::syntax as color;
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Keyword,
    Comment,
    String,
    Number,
    Literal,
    Function,
    Class,
    Type,
    Tag,
    Attribute,
    Property,
    Operator,
    Punctuation,
    Meta,
    Heading,
    Added,
    Removed,
}

impl Role {
    fn color_for(self, palette: &paseo_desktop::appearance::Palette) -> u32 {
        use crate::theme::syntax_light as light;
        if self == Self::Added {
            return palette.diff_addition;
        }
        if self == Self::Removed {
            return palette.diff_deletion;
        }
        if palette.is_dark {
            return self.color();
        }
        match self {
            Self::Keyword => light::KEYWORD,
            Self::Comment => light::COMMENT,
            Self::String => light::STRING,
            Self::Number => light::NUMBER,
            Self::Literal => light::LITERAL,
            Self::Function => light::FUNCTION,
            Self::Class => light::CLASS,
            Self::Type => light::TYPE,
            Self::Tag => light::TAG,
            Self::Attribute => light::ATTRIBUTE,
            Self::Property => light::PROPERTY,
            Self::Operator => light::OPERATOR,
            Self::Punctuation => light::PUNCTUATION,
            Self::Meta => light::META,
            Self::Heading => light::HEADING,
            Self::Added | Self::Removed => unreachable!(),
        }
    }

    fn color(self) -> u32 {
        match self {
            Self::Keyword => color::KEYWORD,
            Self::Comment => color::COMMENT,
            Self::String => color::STRING,
            Self::Number => color::NUMBER,
            Self::Literal => color::LITERAL,
            Self::Function => color::FUNCTION,
            Self::Class => color::CLASS,
            Self::Type => color::TYPE,
            Self::Tag => color::TAG,
            Self::Attribute => color::ATTRIBUTE,
            Self::Property => color::PROPERTY,
            Self::Operator => color::OPERATOR,
            Self::Punctuation => color::PUNCTUATION,
            Self::Meta => color::META,
            Self::Heading => color::HEADING,
            Self::Added => 0x3fb950,
            Self::Removed => 0xf85149,
        }
    }
}

struct Language {
    keywords: &'static [&'static str],
    literals: &'static [&'static str],
    types: &'static [&'static str],
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    quotes: &'static [u8],
    triple_quotes: bool,
    preprocessor: bool,
    upper_case_types: bool,
}

impl Language {
    const PLAIN: Self = Self {
        keywords: &[],
        literals: &[],
        types: &[],
        line_comments: &[],
        block_comment: None,
        quotes: b"\"'",
        triple_quotes: false,
        preprocessor: false,
        upper_case_types: false,
    };
}

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
    "return", "self", "Self", "static", "struct", "super", "trait", "type", "unsafe", "use",
    "where", "while", "yield",
];
const RUST_TYPES: &[&str] = &[
    "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize", "str", "u8", "u16",
    "u32", "u64", "u128", "usize", "String", "Vec", "Option", "Result", "Box", "Arc", "Rc",
];
const RUST_LITERALS: &[&str] = &["true", "false", "None", "Some", "Ok", "Err"];

const TS_KEYWORDS: &[&str] = &[
    "abstract",
    "as",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "declare",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "finally",
    "for",
    "from",
    "function",
    "get",
    "if",
    "implements",
    "import",
    "in",
    "infer",
    "instanceof",
    "interface",
    "keyof",
    "let",
    "namespace",
    "new",
    "of",
    "private",
    "protected",
    "public",
    "readonly",
    "return",
    "satisfies",
    "set",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "try",
    "type",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
];
const TS_LITERALS: &[&str] = &["true", "false", "null", "undefined", "NaN", "Infinity"];

const PY_KEYWORDS: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda",
    "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield",
];
const PY_LITERALS: &[&str] = &["True", "False", "None", "self", "cls"];

const GO_KEYWORDS: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "type",
    "var",
];
const GO_TYPES: &[&str] = &[
    "bool",
    "byte",
    "complex64",
    "complex128",
    "error",
    "float32",
    "float64",
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "rune",
    "string",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "uintptr",
    "any",
];
const GO_LITERALS: &[&str] = &["true", "false", "nil", "iota"];

const C_KEYWORDS: &[&str] = &[
    "alignas",
    "alignof",
    "asm",
    "auto",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "constexpr",
    "continue",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "explicit",
    "export",
    "extern",
    "for",
    "friend",
    "goto",
    "if",
    "inline",
    "namespace",
    "new",
    "operator",
    "private",
    "protected",
    "public",
    "register",
    "return",
    "sizeof",
    "static",
    "struct",
    "switch",
    "template",
    "this",
    "throw",
    "try",
    "typedef",
    "typename",
    "union",
    "using",
    "virtual",
    "volatile",
    "while",
];
const C_TYPES: &[&str] = &[
    "bool", "char", "double", "float", "int", "long", "short", "signed", "size_t", "ssize_t",
    "unsigned", "void", "wchar_t", "int8_t", "int16_t", "int32_t", "int64_t", "uint8_t",
    "uint16_t", "uint32_t", "uint64_t", "std", "string", "vector",
];
const C_LITERALS: &[&str] = &["true", "false", "nullptr", "NULL"];

const JVM_KEYWORDS: &[&str] = &[
    "abstract",
    "assert",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "final",
    "finally",
    "float",
    "for",
    "if",
    "implements",
    "import",
    "instanceof",
    "int",
    "interface",
    "long",
    "native",
    "new",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "static",
    "strictfp",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "transient",
    "try",
    "void",
    "volatile",
    "while",
    "var",
    "record",
    "sealed",
    "permits",
    "yield",
    "fun",
    "val",
    "internal",
    "object",
    "open",
    "override",
    "when",
    "data",
    "companion",
];
const JVM_LITERALS: &[&str] = &["true", "false", "null", "this", "super"];

const SH_KEYWORDS: &[&str] = &[
    "case", "do", "done", "elif", "else", "esac", "fi", "for", "function", "if", "in", "select",
    "then", "time", "until", "while", "export", "local", "readonly", "shift", "source", "return",
    "exit", "set", "unset", "alias", "cd", "echo", "printf", "read", "test",
];
const SH_LITERALS: &[&str] = &["true", "false"];

const SQL_KEYWORDS: &[&str] = &[
    "add",
    "all",
    "alter",
    "and",
    "any",
    "as",
    "asc",
    "begin",
    "between",
    "by",
    "case",
    "cast",
    "column",
    "commit",
    "constraint",
    "create",
    "cross",
    "database",
    "default",
    "delete",
    "desc",
    "distinct",
    "drop",
    "else",
    "end",
    "except",
    "exists",
    "foreign",
    "from",
    "full",
    "group",
    "having",
    "if",
    "in",
    "index",
    "inner",
    "insert",
    "intersect",
    "into",
    "is",
    "join",
    "key",
    "left",
    "like",
    "limit",
    "not",
    "null",
    "offset",
    "on",
    "or",
    "order",
    "outer",
    "primary",
    "references",
    "returning",
    "right",
    "rollback",
    "select",
    "set",
    "table",
    "then",
    "union",
    "unique",
    "update",
    "using",
    "values",
    "view",
    "when",
    "where",
    "with",
];
const SQL_LITERALS: &[&str] = &["true", "false", "null"];

const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "for", "function", "goto", "if", "in", "local",
    "not", "or", "repeat", "return", "then", "until", "while",
];
const LUA_LITERALS: &[&str] = &["true", "false", "nil", "self"];

const RUBY_KEYWORDS: &[&str] = &[
    "alias", "and", "begin", "break", "case", "class", "def", "defined?", "do", "else", "elsif",
    "end", "ensure", "for", "if", "in", "module", "next", "not", "or", "redo", "require", "rescue",
    "retry", "return", "self", "super", "then", "undef", "unless", "until", "when", "while",
    "yield",
];
const RUBY_LITERALS: &[&str] = &["true", "false", "nil"];

const SWIFT_KEYWORDS: &[&str] = &[
    "associatedtype",
    "class",
    "deinit",
    "enum",
    "extension",
    "fileprivate",
    "func",
    "import",
    "init",
    "inout",
    "internal",
    "let",
    "open",
    "operator",
    "private",
    "protocol",
    "public",
    "rethrows",
    "static",
    "struct",
    "subscript",
    "typealias",
    "var",
    "break",
    "case",
    "continue",
    "default",
    "defer",
    "do",
    "else",
    "fallthrough",
    "for",
    "guard",
    "if",
    "in",
    "repeat",
    "return",
    "switch",
    "where",
    "while",
    "as",
    "catch",
    "is",
    "super",
    "throw",
    "throws",
    "try",
];
const SWIFT_LITERALS: &[&str] = &["true", "false", "nil", "self", "Self"];

const PHP_KEYWORDS: &[&str] = &[
    "abstract",
    "and",
    "array",
    "as",
    "break",
    "callable",
    "case",
    "catch",
    "class",
    "clone",
    "const",
    "continue",
    "declare",
    "default",
    "do",
    "echo",
    "else",
    "elseif",
    "empty",
    "endfor",
    "endforeach",
    "endif",
    "endswitch",
    "endwhile",
    "eval",
    "exit",
    "extends",
    "final",
    "finally",
    "fn",
    "for",
    "foreach",
    "function",
    "global",
    "goto",
    "if",
    "implements",
    "include",
    "instanceof",
    "interface",
    "isset",
    "list",
    "match",
    "namespace",
    "new",
    "or",
    "print",
    "private",
    "protected",
    "public",
    "readonly",
    "require",
    "return",
    "static",
    "switch",
    "throw",
    "trait",
    "try",
    "unset",
    "use",
    "var",
    "while",
    "xor",
    "yield",
];
const PHP_LITERALS: &[&str] = &["true", "false", "null"];

const NIX_KEYWORDS: &[&str] = &[
    "assert", "else", "if", "in", "inherit", "let", "or", "rec", "then", "with",
];

const ELIXIR_KEYWORDS: &[&str] = &[
    "after",
    "alias",
    "and",
    "case",
    "catch",
    "cond",
    "def",
    "defmodule",
    "defp",
    "do",
    "else",
    "end",
    "fn",
    "for",
    "if",
    "import",
    "in",
    "not",
    "or",
    "require",
    "rescue",
    "try",
    "unless",
    "use",
    "when",
    "with",
];
const HASKELL_KEYWORDS: &[&str] = &[
    "case", "class", "data", "default", "deriving", "do", "else", "foreign", "if", "import", "in",
    "infix", "instance", "let", "module", "newtype", "of", "then", "type", "where",
];

fn language(info: &str) -> Language {
    let id = info
        .trim()
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();
    let id = id.trim_start_matches('.');
    let id = match id {
        "rust" | "rs" => "rs",
        "typescript" | "tsx" => "ts",
        "javascript" | "jsx" | "mjs" | "cjs" => "js",
        "python" => "py",
        "golang" => "go",
        "c++" => "cpp",
        "c#" | "csharp" => "cs",
        "objective-c" | "objc" => "m",
        "shell" | "shellscript" | "zsh" | "console" => "sh",
        "yml" => "yaml",
        "markdown" | "mdx" => "md",
        "docker" => "dockerfile",
        other => other,
    };
    match id {
        "rs" => Language {
            keywords: RUST_KEYWORDS,
            literals: RUST_LITERALS,
            types: RUST_TYPES,
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            upper_case_types: true,
            ..Language::PLAIN
        },
        "ts" | "js" | "tsx" | "jsx" => Language {
            keywords: TS_KEYWORDS,
            literals: TS_LITERALS,
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            quotes: b"\"'`",
            upper_case_types: true,
            ..Language::PLAIN
        },
        "py" => Language {
            keywords: PY_KEYWORDS,
            literals: PY_LITERALS,
            line_comments: &["#"],
            triple_quotes: true,
            upper_case_types: true,
            ..Language::PLAIN
        },
        "go" => Language {
            keywords: GO_KEYWORDS,
            literals: GO_LITERALS,
            types: GO_TYPES,
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            quotes: b"\"'`",
            upper_case_types: true,
            ..Language::PLAIN
        },
        "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "m" | "mm" => Language {
            keywords: C_KEYWORDS,
            literals: C_LITERALS,
            types: C_TYPES,
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            preprocessor: true,
            ..Language::PLAIN
        },
        "java" | "kt" | "scala" | "cs" => Language {
            keywords: JVM_KEYWORDS,
            literals: JVM_LITERALS,
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            quotes: b"\"'`",
            upper_case_types: true,
            ..Language::PLAIN
        },
        "sh" | "bash" => Language {
            keywords: SH_KEYWORDS,
            literals: SH_LITERALS,
            line_comments: &["#"],
            ..Language::PLAIN
        },
        "sql" => Language {
            keywords: SQL_KEYWORDS,
            literals: SQL_LITERALS,
            line_comments: &["--"],
            block_comment: Some(("/*", "*/")),
            quotes: b"\"'`",
            ..Language::PLAIN
        },
        "lua" => Language {
            keywords: LUA_KEYWORDS,
            literals: LUA_LITERALS,
            line_comments: &["--"],
            ..Language::PLAIN
        },
        "rb" => Language {
            keywords: RUBY_KEYWORDS,
            literals: RUBY_LITERALS,
            line_comments: &["#"],
            upper_case_types: true,
            ..Language::PLAIN
        },
        "swift" => Language {
            keywords: SWIFT_KEYWORDS,
            literals: SWIFT_LITERALS,
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            triple_quotes: true,
            upper_case_types: true,
            ..Language::PLAIN
        },
        "php" => Language {
            keywords: PHP_KEYWORDS,
            literals: PHP_LITERALS,
            line_comments: &["//", "#"],
            block_comment: Some(("/*", "*/")),
            upper_case_types: true,
            ..Language::PLAIN
        },
        "json" | "jsonc" => Language {
            literals: &["true", "false", "null"],
            line_comments: &["//"],
            block_comment: Some(("/*", "*/")),
            quotes: b"\"",
            ..Language::PLAIN
        },
        "yaml" | "yml" => Language {
            literals: &["true", "false", "null", "yes", "no", "on", "off"],
            line_comments: &["#"],
            ..Language::PLAIN
        },
        "toml" | "ini" | "cfg" | "conf" => Language {
            literals: &["true", "false"],
            line_comments: &["#"],
            ..Language::PLAIN
        },
        "nix" => Language {
            keywords: NIX_KEYWORDS,
            literals: &["true", "false", "null"],
            line_comments: &["#"],
            block_comment: Some(("/*", "*/")),
            ..Language::PLAIN
        },
        "ex" => Language {
            keywords: ELIXIR_KEYWORDS,
            literals: &["true", "false", "nil"],
            line_comments: &["#"],
            triple_quotes: true,
            upper_case_types: true,
            ..Language::PLAIN
        },
        "hs" => Language {
            keywords: HASKELL_KEYWORDS,
            literals: &["True", "False", "Nothing", "Just"],
            line_comments: &["--"],
            block_comment: Some(("{-", "-}")),
            upper_case_types: true,
            ..Language::PLAIN
        },
        "css" | "scss" | "less" => Language {
            keywords: &["important", "media", "supports", "keyframes", "import"],
            block_comment: Some(("/*", "*/")),
            ..Language::PLAIN
        },
        "dockerfile" => Language {
            keywords: &[
                "ADD",
                "ARG",
                "CMD",
                "COPY",
                "ENTRYPOINT",
                "ENV",
                "EXPOSE",
                "FROM",
                "HEALTHCHECK",
                "LABEL",
                "MAINTAINER",
                "ONBUILD",
                "RUN",
                "SHELL",
                "STOPSIGNAL",
                "USER",
                "VOLUME",
                "WORKDIR",
            ],
            line_comments: &["#"],
            ..Language::PLAIN
        },
        _ => Language::PLAIN,
    }
}

fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80
}

fn is_identifier(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

/// Colors for the complete source. Adjacent equal roles are merged; gaps keep the default.
#[cfg(test)]
pub fn runs(code: &str, info: &str) -> Vec<(Range<usize>, u32)> {
    let palette =
        paseo_desktop::appearance::resolve(paseo_desktop::settings::ThemePreference::Dark, true);
    styled_runs(code, info, &palette)
}

pub fn styled_runs(
    code: &str,
    info: &str,
    palette: &paseo_desktop::appearance::Palette,
) -> Vec<(Range<usize>, u32)> {
    let mut out: Vec<(Range<usize>, u32)> = Vec::new();
    for (range, role) in tokens(code, info) {
        if range.is_empty() {
            continue;
        }
        let color = role.color_for(palette);
        if let Some((last_range, last_color)) = out.last_mut()
            && last_range.end == range.start
            && *last_color == color
        {
            last_range.end = range.end;
            continue;
        }
        out.push((range, color));
    }
    out
}

/// Non-overlapping lexical tokens in source order. Whitespace and unknown identifiers are
/// omitted; callers paint those with the default color.
pub fn tokens(code: &str, info: &str) -> Vec<(Range<usize>, Role)> {
    let id = info
        .trim()
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();
    match id.trim_start_matches('.') {
        "diff" | "patch" => diff_tokens(code),
        "html" | "xml" | "svg" | "vue" | "svelte" => markup_tokens(code),
        "md" | "markdown" => markdown_tokens(code),
        _ => lex(code, &language(info)),
    }
}

fn lex(code: &str, language: &Language) -> Vec<(Range<usize>, Role)> {
    let bytes = code.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    let mut at_line_start = true;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\n' {
            at_line_start = true;
            index += 1;
            continue;
        }
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if language.preprocessor && at_line_start && byte == b'#' {
            let end = line_end(code, index);
            out.push((index..end, Role::Meta));
            index = end;
            continue;
        }
        if language
            .line_comments
            .iter()
            .any(|prefix| code[index..].starts_with(*prefix))
        {
            let end = line_end(code, index);
            out.push((index..end, Role::Comment));
            index = end;
            continue;
        }
        if let Some((open, close)) = language.block_comment
            && code[index..].starts_with(open)
        {
            let end = code[index + open.len()..]
                .find(close)
                .map_or(code.len(), |offset| {
                    index + open.len() + offset + close.len()
                });
            out.push((index..end, Role::Comment));
            index = end;
            continue;
        }
        if language.triple_quotes
            && (code[index..].starts_with("\"\"\"") || code[index..].starts_with("'''"))
        {
            let delimiter = &code[index..index + 3];
            let end = code[index + 3..]
                .find(delimiter)
                .map_or(code.len(), |offset| index + 3 + offset + 3);
            out.push((index..end, Role::String));
            index = end;
            at_line_start = false;
            continue;
        }
        if byte == b'"' || byte == b'\'' || (language.quotes.contains(&b'`') && byte == b'`') {
            let end = string_end(bytes, index, byte);
            out.push((index..end, Role::String));
            index = end;
            at_line_start = false;
            continue;
        }
        if byte.is_ascii_digit() {
            let end = number_end(bytes, index);
            out.push((index..end, Role::Number));
            index = end;
            at_line_start = false;
            continue;
        }
        if is_identifier_start(byte) {
            let start = index;
            index += 1;
            while index < bytes.len() && is_identifier(bytes[index]) {
                index += 1;
            }
            let word = &code[start..index];
            let role = if language.keywords.contains(&word) {
                Some(Role::Keyword)
            } else if language.literals.contains(&word) {
                Some(Role::Literal)
            } else if language.types.contains(&word) {
                Some(Role::Type)
            } else {
                classify_identifier(bytes, index, word, language)
            };
            if let Some(role) = role {
                out.push((start..index, role));
            }
            at_line_start = false;
            continue;
        }
        let role = operator_role(byte);
        let start = index;
        index += 1;
        if let Some(role) = role {
            while index < bytes.len() {
                let next = bytes[index];
                if next.is_ascii_whitespace() || is_identifier_start(next) || next.is_ascii_digit()
                {
                    break;
                }
                if operator_role(next) != Some(role) {
                    break;
                }
                index += 1;
            }
            out.push((start..index, role));
        }
        at_line_start = false;
    }
    out
}

fn classify_identifier(
    bytes: &[u8],
    index: usize,
    word: &str,
    language: &Language,
) -> Option<Role> {
    let mut next = index;
    while next < bytes.len() && (bytes[next] == b' ' || bytes[next] == b'\t') {
        next += 1;
    }
    match bytes.get(next).copied() {
        Some(b'(' | b'!') => Some(Role::Function),
        Some(b':') if language.upper_case_types && word.len() > 1 => Some(Role::Property),
        _ if language.upper_case_types && word.chars().next().is_some_and(char::is_uppercase) => {
            Some(Role::Class)
        }
        _ => None,
    }
}

fn line_end(code: &str, start: usize) -> usize {
    code[start..]
        .find('\n')
        .map_or(code.len(), |offset| start + offset)
}

fn string_end(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            byte if byte == quote => return index + 1,
            b'\n' if quote != b'`' => return index,
            _ => index += 1,
        }
    }
    bytes.len()
}

fn number_end(bytes: &[u8], start: usize) -> usize {
    let mut index = start;
    if bytes.len() >= index + 2
        && bytes[index] == b'0'
        && matches!(bytes[index + 1], b'x' | b'X' | b'b' | b'B' | b'o' | b'O')
    {
        index += 2;
        while index < bytes.len() && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
        {
            index += 1;
        }
        return index;
    }
    while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'_') {
        index += 1;
    }
    if index < bytes.len()
        && bytes[index] == b'.'
        && bytes
            .get(index + 1)
            .is_some_and(|byte| byte.is_ascii_digit())
    {
        index += 1;
        while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'_') {
            index += 1;
        }
    }
    if index < bytes.len() && matches!(bytes[index], b'e' | b'E') {
        let mut exponent = index + 1;
        if bytes
            .get(exponent)
            .is_some_and(|byte| matches!(byte, b'+' | b'-'))
        {
            exponent += 1;
        }
        if bytes
            .get(exponent)
            .is_some_and(|byte| byte.is_ascii_digit())
        {
            index = exponent;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
        }
    }
    while index < bytes.len()
        && matches!(
            bytes[index],
            b'f' | b'F' | b'u' | b'U' | b'l' | b'L' | b'i' | b'I'
        )
    {
        index += 1;
    }
    index
}

fn operator_role(byte: u8) -> Option<Role> {
    if matches!(
        byte,
        b'(' | b')' | b'[' | b']' | b'{' | b'}' | b',' | b';' | b'.'
    ) {
        return Some(Role::Punctuation);
    }
    if matches!(
        byte,
        b'+' | b'-'
            | b'*'
            | b'/'
            | b'%'
            | b'='
            | b'!'
            | b'<'
            | b'>'
            | b'&'
            | b'|'
            | b'^'
            | b'~'
            | b'?'
            | b':'
            | b'#'
            | b'@'
            | b'\\'
    ) {
        return Some(Role::Operator);
    }
    None
}

fn diff_tokens(code: &str) -> Vec<(Range<usize>, Role)> {
    let mut out = Vec::new();
    let mut start = 0;
    for line in code.split_inclusive('\n') {
        let content = line.trim_end_matches('\n');
        let range = start..start + content.len();
        if content.starts_with("+++") || content.starts_with("---") || content.starts_with("diff ")
        {
            out.push((range, Role::Meta));
        } else if content.starts_with('+') {
            out.push((range, Role::Added));
        } else if content.starts_with('-') {
            out.push((range, Role::Removed));
        } else if content.starts_with("@@") {
            out.push((range, Role::Heading));
        }
        start += line.len();
    }
    out
}

fn markup_tokens(code: &str) -> Vec<(Range<usize>, Role)> {
    let bytes = code.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if code[index..].starts_with("<!--") {
            let end = code[index + 4..]
                .find("-->")
                .map_or(code.len(), |offset| index + 4 + offset + 3);
            out.push((index..end, Role::Comment));
            index = end;
            continue;
        }
        if bytes[index] != b'<' {
            index += 1;
            continue;
        }
        index += 1;
        if bytes.get(index) == Some(&b'/') {
            index += 1;
        }
        let name_start = index;
        while index < bytes.len()
            && (bytes[index].is_ascii_alphanumeric() || matches!(bytes[index], b'-' | b':' | b'_'))
        {
            index += 1;
        }
        if index > name_start {
            out.push((name_start..index, Role::Tag));
        }
        while index < bytes.len() && bytes[index] != b'>' {
            if bytes[index].is_ascii_whitespace() {
                index += 1;
                continue;
            }
            let attribute_start = index;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric()
                    || matches!(bytes[index], b'-' | b':' | b'_' | b'@'))
            {
                index += 1;
            }
            if index > attribute_start {
                out.push((attribute_start..index, Role::Attribute));
            }
            while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            if bytes.get(index) == Some(&b'=') {
                index += 1;
                while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                    index += 1;
                }
                if let Some(quote) = bytes
                    .get(index)
                    .copied()
                    .filter(|byte| matches!(byte, b'"' | b'\''))
                {
                    let end = string_end(bytes, index, quote);
                    out.push((index..end, Role::String));
                    index = end;
                }
            } else if index == attribute_start {
                index += 1;
            }
        }
        if index < bytes.len() {
            index += 1;
        }
    }
    out
}

fn markdown_tokens(code: &str) -> Vec<(Range<usize>, Role)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut fence: Option<&str> = None;
    for line in code.split_inclusive('\n') {
        let content = line.trim_end_matches('\n');
        let range = start..start + content.len();
        let trimmed = content.trim_start();
        if let Some(marker) = fence {
            if trimmed.starts_with(marker) {
                fence = None;
                out.push((range, Role::Meta));
            }
        } else if trimmed.starts_with("```") {
            fence = Some("```");
            out.push((range, Role::Meta));
        } else if trimmed.starts_with("~~~") {
            fence = Some("~~~");
            out.push((range, Role::Meta));
        } else if trimmed.starts_with('#') {
            out.push((range, Role::Heading));
        } else if trimmed.starts_with('>') {
            out.push((range, Role::Meta));
        } else if trimmed.starts_with("- ")
            || trimmed.starts_with("* ")
            || trimmed.starts_with("+ ")
        {
            let marker = start + content.len() - trimmed.len();
            out.push((marker..marker + 1, Role::Punctuation));
        }
        start += line.len();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role_at(tokens: &[(Range<usize>, Role)], code: &str, needle: &str) -> Option<Role> {
        let at = code.find(needle)?;
        tokens
            .iter()
            .find(|(range, _)| range.start <= at && at < range.end)
            .map(|(_, role)| *role)
    }

    #[test]
    fn light_and_dark_syntax_use_authored_token_colors_and_diff_roles() {
        use paseo_desktop::{appearance::resolve, settings::ThemePreference};
        let light = resolve(ThemePreference::Light, false);
        let dark = resolve(ThemePreference::Dark, true);
        assert_eq!(Role::String.color_for(&light), 0x0a3069);
        assert_eq!(Role::String.color_for(&dark), 0xa5d6ff);
        assert_eq!(Role::Keyword.color_for(&light), 0xcf222e);
        assert_eq!(Role::Added.color_for(&light), light.diff_addition);
        assert_eq!(Role::Removed.color_for(&dark), dark.diff_deletion);
        let code = "let message = \"hello\";";
        let at = code.find('"').unwrap();
        let token = styled_runs(code, "rust", &light)
            .into_iter()
            .find(|(range, _)| range.contains(&at))
            .unwrap();
        assert_eq!(token.1, 0x0a3069);
    }

    #[test]
    fn rust_and_typescript_roles_cover_their_lexical_classes() {
        let code = "fn main() {\n    // note\n    let s = \"x\";\n    let n = 0x1f;\n}\n";
        let lexed = tokens(code, "rust");
        assert_eq!(role_at(&lexed, code, "fn"), Some(Role::Keyword));
        assert_eq!(role_at(&lexed, code, "main"), Some(Role::Function));
        assert_eq!(role_at(&lexed, code, "// note"), Some(Role::Comment));
        assert_eq!(role_at(&lexed, code, "\"x\""), Some(Role::String));
        assert_eq!(role_at(&lexed, code, "0x1f"), Some(Role::Number));
        let code = "export function run(value: string) {\n  return true;\n}\n";
        let lexed = tokens(code, "typescript");
        assert_eq!(role_at(&lexed, code, "export"), Some(Role::Keyword));
        assert_eq!(role_at(&lexed, code, "run"), Some(Role::Function));
        assert_eq!(role_at(&lexed, code, "true"), Some(Role::Literal));
        assert_eq!(role_at(&lexed, code, "string"), None);
    }

    #[test]
    fn incomplete_strings_and_unknown_languages_never_panic_or_invent_tokens() {
        assert_eq!(tokens("let x = \"unterminated", "rust").len(), 3);
        assert!(
            tokens("{\"a\": [1, true]}", "json")
                .iter()
                .any(|(_, role)| *role == Role::Literal)
        );
        assert_eq!(tokens("future syntax", "future-language"), vec![]);
        assert_eq!(tokens("hello `code`\n", ""), vec![]);
        let multiline = "x = '''line\nline'''\ny = 1\n";
        let tokens = tokens(multiline, "python");
        assert_eq!(
            role_at(&tokens, multiline, "line\nline"),
            Some(Role::String)
        );
    }

    #[test]
    fn markup_diff_and_runs_preserve_source_offsets() {
        let html = "<div class=\"row\">text</div>";
        let lexed = tokens(html, "html");
        assert_eq!(role_at(&lexed, html, "div"), Some(Role::Tag));
        assert_eq!(role_at(&lexed, html, "class"), Some(Role::Attribute));
        assert_eq!(role_at(&lexed, html, "\"row\""), Some(Role::String));
        let diff = "-old\n+new\n unchanged\n";
        let lexed = tokens(diff, "diff");
        assert_eq!(role_at(&lexed, diff, "-old"), Some(Role::Removed));
        assert_eq!(role_at(&lexed, diff, "+new"), Some(Role::Added));
        let code = "let a = 1;\nlet b = 2;\n";
        let runs = runs(code, "rust");
        assert_eq!(runs.first().unwrap().0.start, 0);
        assert_eq!(runs.last().unwrap().0.end, code.len() - 1);
        for pair in runs.windows(2) {
            assert!(pair[0].0.end <= pair[1].0.start);
        }
    }
}
