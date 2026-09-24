//! Source projections for languages that embed another grammar.

use std::borrow::Cow;

use crate::model::SourceLanguage;

/// Return source suitable for tree-sitter while preserving line positions.
///
/// Razor is intentionally conservative: markup is masked, while `@code` and
/// `@functions` blocks are wrapped in synthetic classes so ordinary C#
/// declarations remain visible to the shared symbol/graph walkers.
pub fn parser_source<'a>(language: SourceLanguage, source: &'a str) -> Cow<'a, str> {
    match language {
        SourceLanguage::Razor => Cow::Owned(razor_csharp_projection(source)),
        _ => Cow::Borrowed(source),
    }
}

fn spaces_preserving_newline(line: &str) -> String {
    line.chars()
        .map(|ch| if ch == '\n' || ch == '\r' { ch } else { ' ' })
        .collect()
}

fn copy_braced_tail(line: &str, start: usize, depth: &mut i32, out: &mut String) {
    for ch in line[start..].chars() {
        match ch {
            '{' => *depth += 1,
            '}' => *depth -= 1,
            _ => {}
        }
        out.push(ch);
    }
}

fn razor_csharp_projection(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut in_block = false;
    let mut depth = 0_i32;
    let mut class_id = 0_u32;

    for line in source.split_inclusive('\n') {
        if in_block {
            copy_braced_tail(line, 0, &mut depth, &mut output);
            if depth <= 0 {
                in_block = false;
                depth = 0;
            }
            continue;
        }

        let trimmed = line.trim_start();
        let is_code_block = trimmed.starts_with("@code") || trimmed.starts_with("@functions");
        if is_code_block {
            let Some(open_rel) = line.find('{') else {
                output.push_str(&spaces_preserving_newline(line));
                continue;
            };
            let indent_len = line.len() - line.trim_start().len();
            let mut prefix = " ".repeat(indent_len);
            let class_name = format!("class __RazorCode{class_id} ");
            class_id += 1;
            prefix.push_str(&class_name);
            output.push_str(&prefix);
            let mut tail = String::new();
            copy_braced_tail(line, open_rel, &mut depth, &mut tail);
            output.push_str(&tail);
            in_block = depth > 0;
            continue;
        }

        if (trimmed.starts_with("@using ") || trimmed.starts_with("@namespace "))
            && let Some(at) = line.find('@')
        {
            output.push_str(&spaces_preserving_newline(&line[..at]));
            output.push(' ');
            output.push_str(&line[at + 1..]);
            continue;
        }

        // Markup, directives, and component attributes are not C#.
        output.push_str(&spaces_preserving_newline(line));
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn razor_projection_keeps_code_lines_and_masks_markup() {
        let source = "<h1>Hello</h1>\n@code {\n    public void Save() {}\n}\n";
        let projected = parser_source(SourceLanguage::Razor, source);
        assert_eq!(projected.lines().count(), source.lines().count());
        assert!(projected.contains("class __RazorCode0"));
        assert!(projected.contains("public void Save"));
        assert!(projected.lines().next().unwrap().trim().is_empty());
    }
}
