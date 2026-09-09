//! Tolerant JSONC parsing shared by doctors and the code-graph resolver.
//!
//! Real-world `tsconfig.json` / editor config files routinely carry `//` and
//! `/* */` comments plus trailing commas. [`parse_jsonc`] strips both before
//! handing the source to `serde_json`, so callers can treat JSONC files as
//! plain JSON values. Extracted from `doctors::typescript_config_hygiene` so
//! the import resolver in [`crate::code_graph`] can reuse the same
//! battle-tested behavior.

use regex::Regex;

/// Parses JSONC source into a `serde_json::Value`.
///
/// Strips `//` and `/* */` comments and trailing commas before parsing.
///
/// # Errors
///
/// Returns the underlying regex or `serde_json` error message when the
/// normalized source still fails to parse.
pub fn parse_jsonc(src: &str) -> Result<serde_json::Value, String> {
    let comments_stripped = strip_json_comments(src);
    let trailing_commas = Regex::new(r",\s*([}\]])").map_err(|err| err.to_string())?;
    let normalized = trailing_commas.replace_all(&comments_stripped, "$1");
    serde_json::from_str::<serde_json::Value>(&normalized).map_err(|err| err.to_string())
}

/// Removes `//` and `/* */` comments while preserving string contents.
///
/// `//` sequences inside double-quoted strings (for example URLs) are kept
/// intact; escapes inside strings are honored.
pub fn strip_json_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut in_string = false;
    let mut escape = false;

    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        if ch == '"' {
            in_string = true;
            out.push(ch);
            continue;
        }

        if ch == '/' {
            match chars.peek().copied() {
                Some('/') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if next == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                    continue;
                }
                Some('*') => {
                    chars.next();
                    let mut previous = '\0';
                    for next in chars.by_ref() {
                        if previous == '*' && next == '/' {
                            break;
                        }
                        previous = next;
                    }
                    continue;
                }
                _ => {}
            }
        }

        out.push(ch);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::{parse_jsonc, strip_json_comments};

    // WHY: `//` inside string values (URLs) must survive comment stripping or
    // tsconfig/editor configs with URLs would silently lose data.
    #[test]
    fn strip_json_comments_keeps_strings_intact() {
        let src =
            "{\n  \"url\": \"https://example.test//keep\",\n  // comment\n  \"value\": 1\n}\n";
        let stripped = strip_json_comments(src);
        assert!(stripped.contains("\"https://example.test//keep\""));
        assert!(!stripped.contains("// comment"));
    }

    // WHY: real tsconfig files carry comments and trailing commas; the shared
    // parser must accept them or every downstream consumer regresses at once.
    #[test]
    fn parse_jsonc_handles_comments_and_trailing_commas() {
        let src = r#"
        {
          // comment
          "compilerOptions": {
            "declaration": true,
            "noEmit": true,
          },
        }
        "#;
        let parsed = parse_jsonc(src).expect("jsonc should parse");
        assert_eq!(
            parsed["compilerOptions"]["declaration"].as_bool(),
            Some(true)
        );
        assert_eq!(parsed["compilerOptions"]["noEmit"].as_bool(), Some(true));
    }

    // WHY: block comments spanning multiple lines are common in tsconfig
    // headers; an unterminated-state bug would corrupt the rest of the file.
    #[test]
    fn strip_json_comments_removes_block_comments() {
        let src = "{ /* multi\nline\ncomment */ \"key\": 2 }";
        let parsed = parse_jsonc(src).expect("jsonc should parse");
        assert_eq!(parsed["key"].as_i64(), Some(2));
    }
}
