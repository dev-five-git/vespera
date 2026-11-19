//! Shared per-crate storage for attribute-macro metadata.
//!
//! `#[route]` and `#[cron]` both accumulate one metadata entry per attribute
//! expansion into a process-global map keyed by
//! [`crate::schema_impl::current_crate_key`], so a long-lived rust-analyzer
//! proc-macro server (one process, many crates) never leaks crate A's entries
//! into crate B. The bookkeeping around that map — poison-tolerant locking,
//! copy-on-write bucket mutation, and replace-insert keyed by
//! `(fn_name, file_path)` — is identical for both, and lives here once.
//!
//! # Key items
//!
//! - [`CrateStorage`] - the static's type: per-crate buckets of `T`
//! - [`SourceIdentified`] - what makes two entries "the same source item"
//! - [`register`] - replace-insert one entry into the current crate's bucket
//! - [`current_crate_items`] - cheap `Arc` snapshot of the current crate's bucket

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

/// Type of a per-crate metadata storage static.
///
/// Keyed by [`crate::schema_impl::current_crate_key`]; each bucket is an
/// [`Arc`] so [`current_crate_items`] hands out a snapshot without deep-cloning
/// every stored entry.
pub type CrateStorage<T> = LazyLock<Mutex<HashMap<String, Arc<Vec<T>>>>>;

/// Metadata that identifies the source item (function) an entry came from.
///
/// Two entries describe the same source item when they share a function name
/// AND a source path (AND a scope, when both know theirs) — what a re-expansion
/// of the same attribute produces, which must replace rather than duplicate
/// the previous entry.
pub trait SourceIdentified {
    /// Name of the annotated function (e.g., `"get_user"`).
    fn fn_name(&self) -> &str;
    /// Source file path, when the compiler exposed one.
    fn file_path(&self) -> Option<&str>;
    /// Where inside the file the item lives (e.g. its inline `mod` path), when
    /// known.  Two entries match only when their scopes are equal; two unknown
    /// scopes match each other, so an unlocated re-expansion still replaces.
    /// An unknown scope never replaces a known one: the item it could be is a
    /// different function of the same name (a `macro_rules!` expansion in
    /// another module), which must be kept beside it.
    fn scope(&self) -> Option<&str>;
}

/// Whether two entries came from the same function in the same file,
/// treating `\` and `/` as equivalent separators (scope not compared).
fn same_fn_and_file<T: SourceIdentified>(left: &T, right: &T) -> bool {
    left.fn_name() == right.fn_name()
        && crate::file_utils::paths_equal_normalized(left.file_path(), right.file_path())
}

/// Whether two entries came from the same function in the same file (and the
/// same scope inside it).
fn same_source<T: SourceIdentified>(left: &T, right: &T) -> bool {
    same_fn_and_file(left, right)
        && match (left.scope(), right.scope()) {
            (Some(left), Some(right)) => left == right,
            (None, None) => true,
            _ => false,
        }
}

/// Whether the IDE's current file text declares `info`'s function exactly
/// once. Only the long-lived server keeps entries of edited-away text, so
/// rustc never asks.
fn single_declaration_in_ide<T: SourceIdentified>(info: &T) -> bool {
    in_long_lived_server()
        && info.file_path().is_some_and(|file| {
            crate::schema_macro::file_cache::get_file_content(std::path::Path::new(file))
                .is_some_and(|content| count_fn_declarations(&content, info.fn_name()) == 1)
        })
}

/// Replace-insert one metadata entry in the current crate's bucket.
///
/// A re-expansion of the same source item (same function name, same normalized
/// file path, and the same scope: two known scopes must be equal, two unknown
/// ones match, a known and an unknown scope do not) overwrites the previous
/// entry; anything else is appended.
///
/// An entry whose scope is unknown next to a known-scope entry of the same
/// function and file is a stale view of that function (an IDE expansion of
/// the unsaved buffer). Inside the IDE, when the file declares the function
/// once, the unknown entry is not kept beside the known one: a later save
/// registers the known scope and would otherwise leave the stale entry behind
/// as a twin, so `vespera!` would mount the function twice. A file declaring
/// the name more than once, and rustc, keep both (a `macro_rules!`
/// expansion of the same name is a different function).
pub fn register<T: Clone + SourceIdentified>(storage: &CrateStorage<T>, info: T) {
    let mut guard = storage.lock().unwrap_or_else(PoisonError::into_inner);
    let bucket = Arc::make_mut(
        guard
            .entry(crate::schema_impl::current_crate_key())
            .or_insert_with(|| Arc::new(Vec::new())),
    );
    if let Some(existing) = bucket
        .iter_mut()
        .find(|existing| same_source(&**existing, &info))
    {
        *existing = info;
        return;
    }
    let rival = |existing: &T| {
        existing.scope().is_some() != info.scope().is_some() && same_fn_and_file(existing, &info)
    };
    if bucket.iter().any(rival) && single_declaration_in_ide(&info) {
        if info.scope().is_none() {
            return;
        }
        bucket
            .retain(|existing| !(existing.scope().is_none() && same_fn_and_file(existing, &info)));
    }
    bucket.push(info);
}

/// Snapshot of the current crate's bucket — a cheap `Arc` clone, so consumers
/// never deep-clone every stored entry.
#[must_use]
pub fn current_crate_items<T>(storage: &CrateStorage<T>) -> Arc<Vec<T>> {
    storage
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&crate::schema_impl::current_crate_key())
        .cloned()
        .unwrap_or_else(|| Arc::new(Vec::new()))
}

/// The entries whose source file still declares their function.
///
/// [`register`] replaces an entry only when the SAME function is expanded
/// again, so in a long-lived rust-analyzer proc-macro server a renamed or
/// deleted handler / job leaves its old entry behind — and `vespera!` would
/// mount a function that no longer exists.  Inside that server, entries are
/// checked against the current file text (the fingerprint-validated
/// `file_cache`, so a file is read once per change); one without a file path
/// or whose file cannot be read is kept.  Under rustc the storage only ever
/// holds the current compilation, so nothing is filtered: a handler / job a
/// `macro_rules!` macro generates (`h!(list)`) has no literal `fn list` in
/// its file and must not be dropped.  Borrows when nothing is stale.
pub fn retain_declared<T: Clone + SourceIdentified>(items: &[T]) -> std::borrow::Cow<'_, [T]> {
    retain_declared_if(items, in_long_lived_server())
}

/// Whether this process is rust-analyzer's long-lived proc-macro server,
/// whose storage keeps entries of edited-away items.
pub fn in_long_lived_server() -> bool {
    static IN_IDE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IN_IDE.get_or_init(|| is_rust_analyzer(std::env::current_exe().ok().as_deref()))
        || test_ide::forced()
}

/// Test-only switch emulating the IDE for [`retain_declared`].
#[cfg(test)]
pub mod test_ide {
    use std::cell::Cell;

    thread_local! {
        static FORCED: Cell<bool> = const { Cell::new(false) };
    }

    pub fn forced() -> bool {
        FORCED.with(Cell::get)
    }

    /// Emulates rust-analyzer on this thread until the guard drops.
    pub fn force() -> impl Drop {
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                FORCED.with(|forced| forced.set(false));
            }
        }
        FORCED.with(|forced| forced.set(true));
        Restore
    }
}

#[cfg(not(test))]
mod test_ide {
    pub(super) const fn forced() -> bool {
        false
    }
}

/// Whether `exe` is rust-analyzer's (long-lived) proc-macro server.
pub fn is_rust_analyzer(exe: Option<&std::path::Path>) -> bool {
    exe.and_then(std::path::Path::file_name)
        .is_some_and(|name| name.to_string_lossy().contains("rust-analyzer"))
}

/// [`retain_declared`], filtering only when `filter` (inside the IDE).
fn retain_declared_if<T: Clone + SourceIdentified>(
    items: &[T],
    filter: bool,
) -> std::borrow::Cow<'_, [T]> {
    let declared = |item: &T| {
        item.file_path().is_none_or(|file| {
            crate::schema_macro::file_cache::get_file_content(std::path::Path::new(file))
                .is_none_or(|content| {
                    declares_fn(&content, item.fn_name())
                        || may_come_from_macro(&content, item.fn_name())
                })
        })
    };
    if !filter || items.iter().all(declared) {
        std::borrow::Cow::Borrowed(items)
    } else {
        std::borrow::Cow::Owned(
            items
                .iter()
                .filter(|item| declared(item))
                .cloned()
                .collect(),
        )
    }
}

/// Whether a macro invocation of `content` names `name` (`h!(list);` may
/// generate `list` from `macro_rules! h`): such a handler / job has no
/// literal `fn name` in its file, so its entry is kept. A name a
/// `macro_rules!` body spells out keeps its `fn name` text and is covered by
/// [`declares_fn`]. Textual like [`declares_fn`], but only the arguments of
/// an `ident!(` / `ident![` / `ident!{` invocation count, outside string
/// literals: `tracing::info!("list called")` does not keep `list`.
fn may_come_from_macro(content: &str, name: &str) -> bool {
    content.lines().any(|line| {
        let code = strip_string_literals(line);
        code.match_indices('!').any(|(bang, _)| {
            // `ident!(`: the `!` follows a name, so `if !(x)` and `a != (b)`
            // are not invocations.
            let follows_name = code[..bang].chars().next_back().is_some_and(is_ident_char);
            let rest = code[bang + 1..].trim_start();
            follows_name
                && rest.starts_with(['(', '[', '{'])
                && contains_ident(invocation_arguments(&rest[1..]), name)
        })
    })
}

/// The text of a macro invocation's argument group, given the text after its
/// opening bracket: up to the matching close bracket, so `log!(x); list();`
/// does not hand `list` to `log!`. An unclosed group (the invocation runs past
/// the line) yields the rest of the line.
fn invocation_arguments(rest: &str) -> &str {
    let mut depth = 0usize;
    for (at, c) in rest.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth == 0 => return &rest[..at],
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    rest
}

/// `line` with the contents of its string literals removed (the quotes, and a
/// raw string's `#`s, stay). Char literals are skipped whole, so the `"` of
/// `'"'` does not open a string. A literal spanning lines is not seen as one.
fn strip_string_literals(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        if let Some(end) = char_literal_end(&chars, i) {
            out.extend(&chars[i..end]);
            i = end;
        } else if let Some((open_end, close_start, end)) = string_literal_span(&chars, i) {
            out.extend(&chars[i..open_end]);
            out.extend(&chars[close_start..end]);
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// The end (exclusive) of the char literal starting at `start` (`'x'`,
/// `'"'`, `'\n'`, `'\''`, `'\u{..}'`); `None` when the quote opens a lifetime
/// or label.
fn char_literal_end(chars: &[char], start: usize) -> Option<usize> {
    if chars.get(start) != Some(&'\'') {
        return None;
    }
    if chars.get(start + 1) == Some(&'\\') {
        let escape = chars.get(start + 3..)?;
        return escape
            .iter()
            .take(10)
            .position(|c| *c == '\'')
            .map(|at| start + 3 + at + 1);
    }
    (chars.get(start + 2) == Some(&'\'')).then_some(start + 3)
}

/// The string literal starting at `start` (`"…"`, or `r#"…"#` with any number
/// of `#`s) as `(open_end, close_start, end)`: `chars[..open_end]` is the
/// opening quote, `chars[close_start..end]` the closing one, and the body
/// between them is dropped. An unterminated literal runs to the end of the
/// line (nothing after its opening quote is kept).
fn string_literal_span(chars: &[char], start: usize) -> Option<(usize, usize, usize)> {
    let (open_end, hashes) = match chars.get(start)? {
        '"' => (start + 1, None),
        'r' if start == 0 || !is_ident_char(chars[start - 1]) => {
            let hashes = chars[start + 1..].iter().take_while(|c| **c == '#').count();
            if chars.get(start + 1 + hashes) != Some(&'"') {
                return None;
            }
            (start + 2 + hashes, Some(hashes))
        }
        _ => return None,
    };
    let len = chars.len();
    let mut at = open_end;
    while at < len {
        match hashes {
            // A raw string has no escapes; it closes at `"` plus its `#`s.
            Some(hashes) if closes_raw(chars, at, hashes) => break,
            None if chars[at] == '"' => break,
            None if chars[at] == '\\' => at += 2,
            _ => at += 1,
        }
    }
    let at = at.min(len);
    if at == len {
        return Some((open_end, len, len));
    }
    Some((open_end, at, at + 1 + hashes.unwrap_or(0)))
}

/// Whether `chars[at..]` is a `"` followed by `hashes` `#`s.
fn closes_raw(chars: &[char], at: usize, hashes: usize) -> bool {
    chars.get(at) == Some(&'"')
        && chars
            .get(at + 1..at + 1 + hashes)
            .is_some_and(|tail| tail.iter().all(|c| *c == '#'))
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether `text` contains `name` as a whole identifier (an unraw `name`
/// also matches `r#name`).
fn contains_ident(text: &str, name: &str) -> bool {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(name).any(|(at, _)| {
        let before = text[..at].chars().next_back().is_none_or(|c| !is_ident(c));
        let after = text[at + name.len()..]
            .chars()
            .next()
            .is_none_or(|c| !is_ident(c));
        before && after
    })
}

/// Whether `content` contains `fn <name>` (the name as a whole identifier,
/// separated from `fn` by whitespace).  Textual on purpose — a cheap check
/// that never parses; a mention in a comment only keeps an entry alive.  An
/// unraw `name` (cron jobs store `r#match` as `match`) also matches its raw
/// spelling `fn r#match`.
fn declares_fn(content: &str, name: &str) -> bool {
    count_fn_declarations(content, name) > 0
}

/// How many times `content` declares `fn <name>` (same matching as
/// [`declares_fn`]).
fn count_fn_declarations(content: &str, name: &str) -> usize {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    content
        .match_indices(name)
        .filter(|&(at, _)| {
            let ends_ident = content[at + name.len()..]
                .chars()
                .next()
                .is_none_or(|c| !is_ident(c));
            let before = &content[..at];
            let before = before.strip_suffix("r#").unwrap_or(before);
            let keyword = before.trim_end();
            ends_ident
                && keyword.len() < before.len()
                && keyword
                    .strip_suffix("fn")
                    .is_some_and(|rest| rest.chars().next_back().is_none_or(|c| !is_ident(c)))
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case::plain("pub async fn get_user() {}", "get_user", true)]
    #[case::newline("fn\n  get_user()", "get_user", true)]
    #[case::raw("pub fn r#type() {}", "r#type", true)]
    #[case::unraw_name_of_raw_fn("pub async fn r#match() {}", "match", true)]
    #[case::raw_without_space("pub fnr#match() {}", "match", false)]
    #[case::at_start_of_file("fn a() {}", "a", true)]
    #[case::renamed("pub async fn get_user_v2() {}", "get_user", false)]
    #[case::prefix_ident("pub async fn my_get_user() {}", "get_user", false)]
    #[case::not_a_declaration("let get_user = 1; get_user();", "get_user", false)]
    #[case::fn_suffix_of_ident("pub async fnx get_user() {}", "get_user", false)]
    #[case::no_space("fn(get_user)", "get_user", false)]
    #[case::absent("", "get_user", false)]
    fn declares_fn_matches_whole_declarations(
        #[case] content: &str,
        #[case] name: &str,
        #[case] expected: bool,
    ) {
        assert_eq!(declares_fn(content, name), expected);
    }

    /// A renamed handler's old entry is dropped; entries without a readable
    /// file are kept, and nothing is cloned when nothing is stale.
    #[test]
    fn retain_declared_drops_entries_whose_function_is_gone() {
        let temp = tempfile::TempDir::new().unwrap();
        let file = temp.path().join("users.rs");
        std::fs::write(&file, "pub async fn renamed() {}").unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        let path = file.display().to_string();
        let items = vec![
            entry("old_name", &path, "stale"),
            entry("renamed", &path, "live"),
            entry("anything", "missing/file.rs", "unreadable"),
            TestEntry {
                file_path: None,
                ..entry("pathless", "", "pathless")
            },
        ];
        let kept = retain_declared_if(&items, true);
        assert!(matches!(kept, std::borrow::Cow::Owned(_)));
        let payloads: Vec<_> = kept.iter().map(|item| item.payload).collect();
        assert_eq!(payloads, ["live", "unreadable", "pathless"]);
        let live = retain_declared_if(&items[1..], true);
        assert!(matches!(live, std::borrow::Cow::Borrowed(_)));
        // Outside the IDE (rustc, this test binary) nothing is filtered: a
        // `macro_rules!`-generated function has no literal `fn name`.
        assert!(
            matches!(retain_declared(&items), std::borrow::Cow::Borrowed(all) if all.len() == 4)
        );
    }

    /// An entry a `macro_rules!` expansion may generate is kept even without a
    /// literal `fn <name>` (the IDE filter must not drop it).
    #[test]
    fn retain_declared_keeps_entries_a_macro_may_generate() {
        let temp = tempfile::TempDir::new().unwrap();
        let file = temp.path().join("generated_routes.rs");
        std::fs::write(
            &file,
            "macro_rules! handler { ($n:ident) => { pub async fn $n() {} } }\nhandler!(list);\n",
        )
        .unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        let path = file.display().to_string();
        let items = vec![
            entry("list", &path, "generated"),
            entry("gone", &path, "stale"),
        ];
        let kept = retain_declared_if(&items, true);
        let payloads: Vec<_> = kept.iter().map(|item| item.payload).collect();
        assert_eq!(payloads, ["generated"]);
    }

    #[test]
    fn retain_declared_drops_a_name_only_a_string_or_a_non_invocation_mentions() {
        let temp = tempfile::TempDir::new().unwrap();
        let file = temp.path().join("renamed_routes.rs");
        std::fs::write(
            &file,
            "#[route(get)]\npub async fn list_all() {}\ntracing::info!(\"list_users called\");\ntracing::info!(\"say \\\"list_users\\\"\");\nlet done = list_users != other;\n",
        )
        .unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        let path = file.display().to_string();
        let items = vec![entry("list_users", &path, "stale")];
        assert!(retain_declared_if(&items, true).is_empty());
    }

    /// A char literal `'"'` does not open a string, so the invocation after it
    /// on the same line still names the generated `list`.
    #[test]
    fn retain_declared_sees_an_invocation_after_a_char_literal_quote() {
        let temp = tempfile::TempDir::new().unwrap();
        let file = temp.path().join("generated_routes.rs");
        std::fs::write(&file, "let q = '\"'; gen!(list);\n").unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        let path = file.display().to_string();
        let items = vec![entry("list", &path, "generated")];
        assert_eq!(retain_declared_if(&items, true).len(), 1);
    }

    /// Only the argument group of an `ident!` invocation may generate the
    /// name: a negation, a `!=`, or a call that follows the invocation on the
    /// same line does not.
    #[rstest::rstest]
    #[case::paren_invocation("gen!(list);", true)]
    #[case::bracket_invocation("gen![list];", true)]
    #[case::brace_invocation("gen! { list }", true)]
    #[case::space_before_group("gen! (list);", true)]
    #[case::nested_group("gen!(a(list));", true)]
    #[case::group_running_past_the_line("gen!(list,", true)]
    #[case::negated_group("if !(list()) {}", false)]
    #[case::call_after_invocation("log!(x); list();", false)]
    #[case::not_equal("let d = list != (other);", false)]
    #[case::bang_after_space("if ! (list()) {}", false)]
    fn may_come_from_macro_names_only_invocation_arguments(
        #[case] line: &str,
        #[case] expected: bool,
    ) {
        assert_eq!(may_come_from_macro(line, "list"), expected);
    }

    #[rstest::rstest]
    #[case::plain_string(r#"say("list")"#, r#"say("")"#)]
    #[case::escaped_quote(r#"say("a\"b") x"#, r#"say("") x"#)]
    #[case::char_quote(r#"let q = '"'; gen!(list);"#, r#"let q = '"'; gen!(list);"#)]
    #[case::escaped_char(r"let q = '\''; x", r"let q = '\''; x")]
    #[case::unicode_char(r"let c = '\u{1F600}'; x", r"let c = '\u{1F600}'; x")]
    #[case::char_escape_at_end(r"let c = '\", r"let c = '\")]
    #[case::lifetime(r"fn f<'a>(x: &'a str) {}", r"fn f<'a>(x: &'a str) {}")]
    #[case::raw_string_backslash(r#"let p = r"C:\"; x"#, r#"let p = r""; x"#)]
    #[case::raw_string_hashes(r##"r#"a "q" b"# x"##, "r#\"\"# x")]
    #[case::raw_ident(r"let r#type = 1;", r"let r#type = 1;")]
    #[case::r_inside_word(r"for x in y {}", r"for x in y {}")]
    #[case::unterminated(r#"say("list"#, r#"say(""#)]
    fn strip_string_literals_keeps_code_and_quotes(#[case] line: &str, #[case] expected: &str) {
        assert_eq!(strip_string_literals(line), expected);
    }

    #[rstest::rstest]
    #[case::server(Some("rust-analyzer-proc-macro-srv"), true)]
    #[case::exe(Some("rust-analyzer.exe"), true)]
    #[case::rustc(Some("rustc"), false)]
    #[case::unknown(None, false)]
    fn is_rust_analyzer_matches_the_server_exe(#[case] exe: Option<&str>, #[case] expected: bool) {
        assert_eq!(is_rust_analyzer(exe.map(std::path::Path::new)), expected);
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TestEntry {
        fn_name: String,
        file_path: Option<String>,
        payload: &'static str,
        scope: Option<&'static str>,
    }

    impl SourceIdentified for TestEntry {
        fn fn_name(&self) -> &str {
            &self.fn_name
        }
        fn file_path(&self) -> Option<&str> {
            self.file_path.as_deref()
        }
        fn scope(&self) -> Option<&str> {
            self.scope
        }
    }

    static TEST_STORAGE: CrateStorage<TestEntry> = LazyLock::new(|| Mutex::new(HashMap::new()));

    fn entry(fn_name: &str, file_path: &str, payload: &'static str) -> TestEntry {
        TestEntry {
            fn_name: fn_name.to_string(),
            file_path: Some(file_path.to_string()),
            payload,
            scope: None,
        }
    }

    fn scoped(scope: Option<&'static str>, payload: &'static str) -> TestEntry {
        TestEntry {
            scope,
            ..entry("__scoped", "C:/vespera/jobs.rs", payload)
        }
    }

    /// Known scopes must match. An unknown scope never replaces a known one (it
    /// is a different function of that name, kept beside it), but two unknown
    /// scopes of one item do replace each other.
    #[test]
    fn register_keys_by_scope_when_both_know_it() {
        register(&TEST_STORAGE, scoped(Some("a"), "a1"));
        register(&TEST_STORAGE, scoped(Some("b"), "b1"));
        assert_eq!(payloads_for("__scoped"), vec!["a1", "b1"]);
        register(&TEST_STORAGE, scoped(Some("b"), "b2"));
        assert_eq!(payloads_for("__scoped"), vec!["a1", "b2"]);
        register(&TEST_STORAGE, scoped(None, "unknown"));
        assert_eq!(payloads_for("__scoped"), vec!["a1", "b2", "unknown"]);
        register(&TEST_STORAGE, scoped(None, "unknown-again"));
        assert_eq!(payloads_for("__scoped"), vec!["a1", "b2", "unknown-again"]);
    }

    fn payloads_for(fn_name: &str) -> Vec<&'static str> {
        current_crate_items(&TEST_STORAGE)
            .iter()
            .filter(|found| found.fn_name == fn_name)
            .map(|found| found.payload)
            .collect()
    }

    #[test]
    fn register_replaces_same_fn_with_separator_equivalent_path() {
        // `\` vs `/` must compare equal, so the second registration REPLACES.
        register(
            &TEST_STORAGE,
            entry("__replace_me", r"C:\vespera\routes\a.rs", "before"),
        );
        register(
            &TEST_STORAGE,
            entry("__replace_me", "C:/vespera/routes/a.rs", "after"),
        );

        assert_eq!(payloads_for("__replace_me"), vec!["after"]);
    }

    #[test]
    fn register_pushes_different_fn_name_at_same_path() {
        register(
            &TEST_STORAGE,
            entry("__push_first", "C:/vespera/routes/b.rs", "first"),
        );
        register(
            &TEST_STORAGE,
            entry("__push_second", "C:/vespera/routes/b.rs", "second"),
        );

        assert_eq!(payloads_for("__push_first"), vec!["first"]);
        assert_eq!(payloads_for("__push_second"), vec!["second"]);
    }

    fn located(
        fn_name: &str,
        file_path: &str,
        scope: Option<&'static str>,
        payload: &'static str,
    ) -> TestEntry {
        TestEntry {
            scope,
            ..entry(fn_name, file_path, payload)
        }
    }

    /// `src/<name>.rs` holding `text`, with the file cache refreshed.
    fn write_source(name: &str, text: &str) -> (tempfile::TempDir, String) {
        let temp = tempfile::TempDir::new().unwrap();
        let file = temp.path().join(name);
        std::fs::write(&file, text).unwrap();
        crate::schema_macro::file_cache::bump_epoch();
        let path = file.display().to_string();
        (temp, path)
    }

    /// Edit without save: the IDE expands the buffer's `ide_list` with an
    /// unknown scope, then the saved file registers it with a known scope.
    /// The stale twin is gone, and so is a later unknown re-expansion.
    #[test]
    fn ide_stale_unknown_scope_entry_does_not_twin_the_saved_one() {
        let (_temp, path) = write_source("ide_once.rs", "pub async fn ide_list() {}");
        let _ide = test_ide::force();
        register(
            &TEST_STORAGE,
            located("ide_list", &path, None, "stale-buffer"),
        );
        register(&TEST_STORAGE, located("ide_list", &path, Some(""), "saved"));
        assert_eq!(payloads_for("ide_list"), vec!["saved"]);
        register(&TEST_STORAGE, located("ide_list", &path, None, "again"));
        assert_eq!(payloads_for("ide_list"), vec!["saved"]);
    }

    /// When the file declares the name twice, an unknown scope may be the
    /// other declaration: both entries stay (IDE).
    #[test]
    fn ide_unknown_scope_entry_is_kept_when_the_name_is_declared_twice() {
        let (_temp, path) = write_source(
            "ide_twice.rs",
            "pub fn twice_list() {}\nmod inner { pub fn twice_list() {} }",
        );
        let _ide = test_ide::force();
        register(&TEST_STORAGE, located("twice_list", &path, None, "unknown"));
        register(
            &TEST_STORAGE,
            located("twice_list", &path, Some(""), "known"),
        );
        assert_eq!(payloads_for("twice_list"), vec!["unknown", "known"]);
    }

    /// Outside the IDE (rustc) the unknown-scope entry is never dropped.
    #[test]
    fn rustc_keeps_unknown_scope_entry_beside_the_known_one() {
        let (_temp, path) = write_source("rustc_once.rs", "pub fn rustc_list() {}");
        register(&TEST_STORAGE, located("rustc_list", &path, None, "unknown"));
        register(
            &TEST_STORAGE,
            located("rustc_list", &path, Some(""), "known"),
        );
        assert_eq!(payloads_for("rustc_list"), vec!["unknown", "known"]);
    }

    #[test]
    fn current_crate_items_is_empty_for_unused_storage() {
        static EMPTY_STORAGE: CrateStorage<TestEntry> =
            LazyLock::new(|| Mutex::new(HashMap::new()));
        assert!(current_crate_items(&EMPTY_STORAGE).is_empty());
    }
}
