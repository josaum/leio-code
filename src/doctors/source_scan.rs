//! Small source scanners shared by doctors that need executable-code evidence.

pub(crate) fn find_awaited_function_call_line(body: &str, function: &str) -> Option<usize> {
    if function.is_empty() {
        return None;
    }

    let code = code_only(body);
    let bytes = code.as_bytes();
    for (index, _) in code.match_indices("await") {
        if index > 0 && is_identifier_byte(bytes[index - 1]) {
            continue;
        }
        let mut cursor = index + "await".len();
        if cursor >= bytes.len() || !bytes[cursor].is_ascii_whitespace() {
            continue;
        }
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if !code[cursor..].starts_with(function) {
            continue;
        }
        cursor += function.len();
        if cursor < bytes.len() && is_identifier_byte(bytes[cursor]) {
            continue;
        }
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if bytes.get(cursor) == Some(&b'(') {
            return Some(code[..index].bytes().filter(|byte| *byte == b'\n').count() + 1);
        }
    }
    None
}

#[derive(Clone, Copy)]
enum ScanState {
    Code,
    LineComment,
    BlockComment,
    SingleQuote,
    DoubleQuote,
    Template,
}

fn code_only(body: &str) -> String {
    let source = body.as_bytes();
    let mut output = source.to_vec();
    let mut state = ScanState::Code;
    let mut index = 0;

    while index < source.len() {
        match state {
            ScanState::Code => match source[index] {
                b'/' if source.get(index + 1) == Some(&b'/') => {
                    output[index] = b' ';
                    output[index + 1] = b' ';
                    state = ScanState::LineComment;
                    index += 2;
                    continue;
                }
                b'/' if source.get(index + 1) == Some(&b'*') => {
                    output[index] = b' ';
                    output[index + 1] = b' ';
                    state = ScanState::BlockComment;
                    index += 2;
                    continue;
                }
                b'\'' => state = ScanState::SingleQuote,
                b'"' => state = ScanState::DoubleQuote,
                b'`' => state = ScanState::Template,
                _ => {
                    index += 1;
                    continue;
                }
            },
            ScanState::LineComment => {
                if source[index] == b'\n' {
                    state = ScanState::Code;
                    index += 1;
                    continue;
                }
            }
            ScanState::BlockComment => {
                if source[index] == b'*' && source.get(index + 1) == Some(&b'/') {
                    output[index] = b' ';
                    output[index + 1] = b' ';
                    state = ScanState::Code;
                    index += 2;
                    continue;
                }
                if source[index] == b'\n' {
                    index += 1;
                    continue;
                }
            }
            ScanState::SingleQuote | ScanState::DoubleQuote | ScanState::Template => {
                let terminator = match state {
                    ScanState::SingleQuote => b'\'',
                    ScanState::DoubleQuote => b'"',
                    ScanState::Template => b'`',
                    _ => unreachable!(),
                };
                if source[index] == b'\\' {
                    output[index] = b' ';
                    if let Some(next) = output.get_mut(index + 1)
                        && *next != b'\n'
                    {
                        *next = b' ';
                    }
                    index += 2;
                    continue;
                }
                if source[index] == terminator {
                    output[index] = b' ';
                    state = ScanState::Code;
                    index += 1;
                    continue;
                }
                if source[index] == b'\n' {
                    index += 1;
                    continue;
                }
            }
        }
        output[index] = b' ';
        index += 1;
    }

    String::from_utf8(output).expect("replacing bytes with ASCII spaces preserves UTF-8")
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
}

#[cfg(test)]
mod tests {
    use super::find_awaited_function_call_line;

    #[test]
    fn rejects_multiline_block_comment_contents() {
        let body = "/*\nconst credential = await resolveEvoCredential({});\n*/";
        assert_eq!(
            find_awaited_function_call_line(body, "resolveEvoCredential"),
            None
        );
    }

    #[test]
    fn rejects_string_and_template_literal_contents() {
        for body in [
            r#"const note = "await resolveEvoCredential({})";"#,
            r#"const note = 'await resolveEvoCredential({})';"#,
            r#"const note = `await resolveEvoCredential({})`;"#,
        ] {
            assert_eq!(
                find_awaited_function_call_line(body, "resolveEvoCredential"),
                None
            );
        }
    }

    #[test]
    fn finds_real_call_and_preserves_line_number() {
        let body = "const note = 'not a call';\nconst credential =\n  await\n    resolveEvoCredential({});";
        assert_eq!(
            find_awaited_function_call_line(body, "resolveEvoCredential"),
            Some(3)
        );
    }

    #[test]
    fn ignores_comment_markers_inside_strings_before_a_real_call() {
        let body = r#"const note = "// not a comment /*";
const credential = await resolveEvoCredential({});"#;
        assert_eq!(
            find_awaited_function_call_line(body, "resolveEvoCredential"),
            Some(2)
        );
    }
}
