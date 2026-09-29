pub type ByteSpan = (usize, usize);

#[derive(Clone, Copy, PartialEq)]
enum Syntax {
    Script,
    Go,
    Rust,
    CFamily,
    Css,
    Hash,
    HashPowershell,
    Markup,
    DashSql,
    DashLua,
}

fn syntax_for_extension(extension: &str) -> Option<Syntax> {
    Some(match extension {
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" | "mts" | "cts" => Syntax::Script,
        "go" => Syntax::Go,
        "rs" => Syntax::Rust,
        "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hh" | "cs" | "java" | "kt" | "kts" | "swift" | "scala" | "dart"
        | "proto" | "groovy" | "gradle" | "zig" | "scss" | "less" | "jsonc" | "json5" | "cu" | "glsl" | "wgsl" => Syntax::CFamily,
        "css" => Syntax::Css,
        "py" | "sh" | "bash" | "zsh" | "yml" | "yaml" | "toml" | "rb" | "pl" | "r" | "mk" | "tf" | "env" => Syntax::Hash,
        "ps1" | "psm1" | "psd1" => Syntax::HashPowershell,
        "html" | "htm" | "xhtml" | "xml" | "svg" | "vue" | "svelte" | "md" | "markdown" => Syntax::Markup,
        "sql" | "hs" => Syntax::DashSql,
        "lua" => Syntax::DashLua,
        _ => return None,
    })
}

pub fn has_comment_syntax(path: &str) -> bool {
    syntax_for_path(path).is_some()
}

fn syntax_for_path(path: &str) -> Option<Syntax> {
    let file_name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    if file_name.eq_ignore_ascii_case("dockerfile") || file_name.eq_ignore_ascii_case("makefile") {
        return Some(Syntax::Hash);
    }
    let extension = file_name.rsplit_once('.')?.1.to_ascii_lowercase();
    syntax_for_extension(&extension)
}

pub fn comment_spans(path: &str, source: &str) -> Option<Vec<ByteSpan>> {
    let syntax = syntax_for_path(path)?;
    let bytes = source.as_bytes();
    let mut spans = Vec::new();
    match syntax {
        Syntax::Markup => markup_spans(bytes, &mut spans),
        Syntax::Css => c_family_spans(bytes, 0, bytes.len(), Syntax::Css, &mut spans),
        Syntax::Hash | Syntax::HashPowershell => hash_spans(bytes, syntax == Syntax::HashPowershell, path, &mut spans),
        Syntax::DashSql | Syntax::DashLua => dash_spans(bytes, syntax, &mut spans),
        other => c_family_spans(bytes, 0, bytes.len(), other, &mut spans),
    }
    Some(spans)
}

pub fn span_contains(spans: &[ByteSpan], offset: usize) -> bool {
    let after = spans.partition_point(|&(start, _)| start <= offset);
    after > 0 && offset < spans[after - 1].1
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

fn find_from(bytes: &[u8], from: usize, end: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from >= end {
        return None;
    }
    bytes[from..end].windows(needle.len()).position(|window| window == needle).map(|p| from + p)
}

fn line_end(bytes: &[u8], from: usize, end: usize) -> usize {
    bytes[from..end].iter().position(|&b| b == b'\n').map(|p| from + p).unwrap_or(end)
}

fn skip_quoted(bytes: &[u8], open: usize, end: usize, quote: u8, spans_lines: bool) -> usize {
    let mut i = open + 1;
    while i < end {
        match bytes[i] {
            b'\\' => i += 2,
            b if b == quote => return i + 1,
            b'\n' if !spans_lines => return i,
            _ => i += 1,
        }
    }
    end
}

fn regex_may_start_after(bytes: &[u8], slash: usize) -> bool {
    let mut i = slash;
    while i > 0 && matches!(bytes[i - 1], b' ' | b'\t' | b'\r' | b'\n') {
        i -= 1;
    }
    if i == 0 {
        return true;
    }
    let previous = bytes[i - 1];
    if !is_identifier_byte(previous) {
        return matches!(previous, b'(' | b',' | b'=' | b':' | b'[' | b'!' | b'&' | b'|' | b'?' | b'{' | b'}' | b';' | b'+' | b'-' | b'*' | b'%' | b'<' | b'>' | b'~' | b'^');
    }
    let mut word_start = i;
    while word_start > 0 && is_identifier_byte(bytes[word_start - 1]) {
        word_start -= 1;
    }
    matches!(&bytes[word_start..i], b"return" | b"typeof" | b"case" | b"in" | b"of" | b"delete" | b"void" | b"throw" | b"new" | b"else" | b"do" | b"yield" | b"await")
}

fn skip_regex_literal(bytes: &[u8], open: usize, end: usize) -> usize {
    let mut i = open + 1;
    let mut in_class = false;
    while i < end {
        match bytes[i] {
            b'\\' => i += 1,
            b'[' => in_class = true,
            b']' => in_class = false,
            b'/' if !in_class => {
                i += 1;
                while i < end && bytes[i].is_ascii_alphabetic() {
                    i += 1;
                }
                return i;
            }
            b'\n' => return open + 1,
            _ => {}
        }
        i += 1;
    }
    open + 1
}

fn rust_raw_string_end(bytes: &[u8], r_at: usize, end: usize) -> Option<usize> {
    let mut i = r_at + 1;
    let mut hashes = 0;
    while i < end && bytes[i] == b'#' {
        hashes += 1;
        i += 1;
    }
    if i >= end || bytes[i] != b'"' {
        return None;
    }
    let mut closing = vec![b'"'];
    closing.extend(std::iter::repeat(b'#').take(hashes));
    Some(find_from(bytes, i + 1, end, &closing).map(|p| p + closing.len()).unwrap_or(end))
}

fn rust_char_literal_end(bytes: &[u8], quote: usize, end: usize) -> Option<usize> {
    if quote + 2 < end && bytes[quote + 1] == b'\\' {
        let close = bytes[quote + 2..end.min(quote + 12)].iter().position(|&b| b == b'\'')?;
        return Some(quote + 2 + close + 1);
    }
    let first_char_len = (1..=4)
        .filter(|&n| quote + 1 + n <= end)
        .find(|&n| std::str::from_utf8(&bytes[quote + 1..quote + 1 + n]).is_ok())?;
    let after = quote + 1 + first_char_len;
    (after < end && bytes[after] == b'\'').then_some(after + 1)
}

fn nested_block_end(bytes: &[u8], open: usize, end: usize) -> usize {
    let mut depth = 0usize;
    let mut i = open;
    while i + 1 < end {
        if bytes[i] == b'/' && bytes[i + 1] == b'*' {
            depth += 1;
            i += 2;
        } else if bytes[i] == b'*' && bytes[i + 1] == b'/' {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return i;
            }
        } else {
            i += 1;
        }
    }
    end
}

fn c_family_spans(bytes: &[u8], from: usize, end: usize, syntax: Syntax, spans: &mut Vec<ByteSpan>) {
    let line_comments = syntax != Syntax::Css;
    let mut i = from;
    while i < end {
        let byte = bytes[i];
        let next = if i + 1 < end { bytes[i + 1] } else { 0 };
        if byte == b'/' && next == b'/' && line_comments {
            let stop = line_end(bytes, i, end);
            spans.push((i, stop));
            i = stop;
        } else if byte == b'/' && next == b'*' {
            let stop = if syntax == Syntax::Rust {
                nested_block_end(bytes, i, end)
            } else {
                find_from(bytes, i + 2, end, b"*/").map(|p| p + 2).unwrap_or(end)
            };
            spans.push((i, stop));
            i = stop;
        } else if byte == b'"' {
            i = skip_quoted(bytes, i, end, b'"', syntax == Syntax::Rust || syntax == Syntax::Go || syntax == Syntax::Css);
        } else if byte == b'\'' {
            i = match syntax {
                Syntax::Rust => rust_char_literal_end(bytes, i, end).unwrap_or(i + 1),
                _ => skip_quoted(bytes, i, end, b'\'', false),
            };
        } else if byte == b'`' && matches!(syntax, Syntax::Script | Syntax::Go) {
            i = skip_quoted(bytes, i, end, b'`', true);
        } else if byte == b'r' && syntax == Syntax::Rust && (i == 0 || !is_identifier_byte(bytes[i - 1]) || bytes[i - 1] == b'b') {
            i = rust_raw_string_end(bytes, i, end).unwrap_or(i + 1);
        } else if byte == b'/' && syntax == Syntax::Script && regex_may_start_after(bytes, i) {
            i = skip_regex_literal(bytes, i, end);
        } else {
            i += 1;
        }
    }
}

fn hash_spans(bytes: &[u8], powershell_blocks: bool, path: &str, spans: &mut Vec<ByteSpan>) {
    let extension = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let hash_needs_leading_space = !matches!(extension.as_str(), "py" | "toml" | "rb" | "pl" | "r" | "ps1" | "psm1" | "psd1");
    let triple_quotes = extension == "py";
    let end = bytes.len();
    let mut i = 0;
    while i < end {
        let byte = bytes[i];
        if powershell_blocks && byte == b'<' && i + 1 < end && bytes[i + 1] == b'#' {
            let stop = find_from(bytes, i + 2, end, b"#>").map(|p| p + 2).unwrap_or(end);
            spans.push((i, stop));
            i = stop;
        } else if byte == b'#' {
            let leading_ok = i == 0 || !hash_needs_leading_space || matches!(bytes[i - 1], b' ' | b'\t' | b'\n' | b'\r' | b';');
            if leading_ok {
                let stop = line_end(bytes, i, end);
                spans.push((i, stop));
                i = stop;
            } else {
                i += 1;
            }
        } else if byte == b'"' || byte == b'\'' {
            if triple_quotes && i + 2 < end && bytes[i + 1] == byte && bytes[i + 2] == byte {
                let closing = [byte, byte, byte];
                i = find_from(bytes, i + 3, end, &closing).map(|p| p + 3).unwrap_or(end);
            } else {
                i = skip_quoted(bytes, i, end, byte, false);
            }
        } else {
            i += 1;
        }
    }
}

fn dash_spans(bytes: &[u8], syntax: Syntax, spans: &mut Vec<ByteSpan>) {
    let end = bytes.len();
    let mut i = 0;
    while i < end {
        let byte = bytes[i];
        let next = if i + 1 < end { bytes[i + 1] } else { 0 };
        if byte == b'-' && next == b'-' {
            let long_bracket = syntax == Syntax::DashLua && i + 3 < end && bytes[i + 2] == b'[' && bytes[i + 3] == b'[';
            let stop = if long_bracket {
                find_from(bytes, i + 4, end, b"]]").map(|p| p + 2).unwrap_or(end)
            } else {
                line_end(bytes, i, end)
            };
            spans.push((i, stop));
            i = stop;
        } else if byte == b'/' && next == b'*' && syntax == Syntax::DashSql {
            let stop = find_from(bytes, i + 2, end, b"*/").map(|p| p + 2).unwrap_or(end);
            spans.push((i, stop));
            i = stop;
        } else if byte == b'\'' || byte == b'"' {
            i = skip_quoted(bytes, i, end, byte, true);
        } else {
            i += 1;
        }
    }
}

fn ascii_lowercase_find(lowered: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    find_from(lowered, from, lowered.len(), needle)
}

fn markup_spans(bytes: &[u8], spans: &mut Vec<ByteSpan>) {
    let lowered: Vec<u8> = bytes.iter().map(|b| b.to_ascii_lowercase()).collect();
    let end = bytes.len();
    let mut i = 0;
    while i < end {
        let comment_at = ascii_lowercase_find(&lowered, i, b"<!--");
        let script_at = ascii_lowercase_find(&lowered, i, b"<script");
        let style_at = ascii_lowercase_find(&lowered, i, b"<style");
        let next_event = [comment_at, script_at, style_at].into_iter().flatten().min();
        let Some(event) = next_event else { return };
        if Some(event) == comment_at {
            let stop = find_from(bytes, event + 4, end, b"-->").map(|p| p + 3).unwrap_or(end);
            spans.push((event, stop));
            i = stop;
            continue;
        }
        let (closing_tag, syntax): (&[u8], Syntax) = if Some(event) == script_at { (b"</script", Syntax::Script) } else { (b"</style", Syntax::Css) };
        let Some(open_end) = find_from(bytes, event, end, b">").map(|p| p + 1) else { return };
        let body_end = ascii_lowercase_find(&lowered, open_end, closing_tag).unwrap_or(end);
        c_family_spans(bytes, open_end, body_end, syntax, spans);
        i = body_end.max(open_end);
    }
}
