pub(super) fn quoted_value_after_key(tokens: &str, key: &str) -> Option<String> {
    for start in key_positions(tokens, key) {
        // Only the `key = "value"` form: a side list `key(serialize = "a",
        // deserialize = "b")` must never yield one side's value for both.
        let Some(value_part) = tokens[start + key.len()..].trim_start().strip_prefix('=') else {
            continue;
        };
        if let Some(value) = leading_string_literal(value_part) {
            return Some(value);
        }
    }
    None
}

/// The value of the string literal `value_part` starts with (after
/// whitespace), plain (`"..."`) or raw (`r"..."`, `r#"..."#`); a value of any
/// other kind yields `None`. A plain literal's closing quote is the first one
/// not escaped by a backslash, a raw literal's is the quote followed by the
/// same number of `#`. The literal is decoded by `syn`, as rustc decodes it.
fn leading_string_literal(value_part: &str) -> Option<String> {
    let text = value_part.trim_start();
    let end = if text.starts_with('"') {
        quoted_literal_end(text)?
    } else {
        raw_literal_end(text)?
    };
    syn::parse_str::<syn::LitStr>(&text[..end])
        .ok()
        .map(|literal| literal.value())
}

/// Byte index just past the plain literal `text` (which starts with `"`).
fn quoted_literal_end(text: &str) -> Option<usize> {
    let mut escaped = false;
    text.char_indices().skip(1).find_map(|(index, ch)| {
        if escaped {
            escaped = false;
            None
        } else if ch == '\\' {
            escaped = true;
            None
        } else {
            (ch == '"').then_some(index + 1)
        }
    })
}

/// Byte index just past the raw literal `text` (`r` + `#`s + `"` ... `"` +
/// the same `#`s); `None` when `text` opens no raw literal.
fn raw_literal_end(text: &str) -> Option<usize> {
    let hashes = raw_open_hashes(text)?;
    let open = 1 + hashes + 1;
    let closing = format!("\"{}", "#".repeat(hashes));
    let close = text[open..].find(&closing)?;
    Some(open + close + closing.len())
}

/// The number of `#`s of the raw literal opener `text` starts with
/// (`r` + `#`s + `"`); `None` when `text` opens no raw literal.
fn raw_open_hashes(text: &str) -> Option<usize> {
    let after_r = text.strip_prefix('r')?;
    let hashes = after_r.len() - after_r.trim_start_matches('#').len();
    after_r[hashes..].starts_with('"').then_some(hashes)
}

/// Whether `ch` can continue an identifier.
fn is_ident_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

pub(super) fn contains_standalone_word(tokens: &str, word: &str) -> bool {
    key_positions(tokens, word).next().is_some()
}

/// Start indices of `word` in `tokens` written as a key: a standalone word
/// or the last segment of a qualified path (`serde::default`), never inside
/// a string literal (`skip_serializing_if = "util::skip_none"` names no key)
/// and not a value (`skip_serializing_if = Option::default` names none).
fn key_positions<'a>(tokens: &'a str, word: &'a str) -> impl Iterator<Item = usize> + 'a {
    tokens
        .match_indices(word)
        .map(|(start, _)| start)
        .filter(move |&start| {
            (is_standalone_word_at(tokens, start, word) || is_qualified_key(tokens, start, word))
                && !in_string_literal(tokens, start)
                && !is_value_position(tokens, start)
        })
}

/// Whether the word at `start` is the value of a `=`, possibly through a
/// `::` path (`Option::default`, `Option :: default`): such a word names a
/// value, not a key.
fn is_value_position(tokens: &str, start: usize) -> bool {
    let mut head = start;
    while let Some(path) = tokens[..head].trim_end().strip_suffix("::") {
        head = path.trim_end().trim_end_matches(is_ident_char).len();
    }
    tokens[..head].trim_end().ends_with('=')
}

/// Whether byte `at` of `tokens` lies inside a string literal, plain
/// (`"..."`, escapes honoured) or raw (`r"..."`, `r#"..."#`, which has no
/// escapes and closes on a quote followed by the same number of `#`s).
fn in_string_literal(tokens: &str, at: usize) -> bool {
    let mut pos = 0;
    while pos < at {
        let text = &tokens[pos..];
        let literal_end = if text.starts_with('"') {
            quoted_literal_end(text)
        } else if raw_open_hashes(text).is_some() && !tokens[..pos].ends_with(is_ident_char) {
            raw_literal_end(text)
        } else {
            pos += text.chars().next().map_or(1, char::len_utf8);
            continue;
        };
        match literal_end {
            Some(len) if pos + len <= at => pos += len,
            _ => return true,
        }
    }
    false
}

/// `word` at `start` is the last segment of a qualified path: `::` before it
/// and a key boundary after it (`a::skip_none` is not `skip`).
fn is_qualified_key(tokens: &str, start: usize, word: &str) -> bool {
    tokens[..start].ends_with("::") && is_key_end(tokens, start + word.len())
}

/// The character at byte `end` ends a key (or the input ends there).
fn is_key_end(tokens: &str, end: usize) -> bool {
    matches!(
        tokens[end..].chars().next(),
        None | Some(' ' | ',' | ')' | '=')
    )
}

fn is_standalone_word_at(tokens: &str, start: usize, word: &str) -> bool {
    matches!(tokens[..start].chars().last(), None | Some(' ' | ',' | '('))
        && is_key_end(tokens, start + word.len())
}

/// `default` / `default = "path"` among raw `#[serde(...)]` tokens, skipping
/// any occurrence that is no key (inside a string literal such as
/// `rename = "defaultValue"`, or part of a longer identifier) and a
/// `default = <non-string>` (not a serde default).
#[allow(clippy::option_option)]
pub(super) fn scan_default_from_raw_tokens(tokens: &str) -> Option<Option<String>> {
    for start in key_positions(tokens, "default") {
        let remaining = tokens[start + "default".len()..].trim_start();
        let Some(after_equals) = remaining.strip_prefix('=') else {
            return Some(None);
        };
        if let Some(path) = leading_string_literal(after_equals) {
            return Some(Some(path));
        }
    }
    None
}

#[cfg(test)]
mod tests;
