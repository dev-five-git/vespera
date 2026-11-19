//! URL path rules shared by every path the macros mount for a client: the
//! documentation routes (`docs_url`, `redoc_url`, `spec_url`) and the export
//! `prefix`.

/// Whether a path segment is one a client resolves away or reads differently:
/// the RFC 3986 dot segments (`.`, `..`, compared case-insensitively), with
/// their percent-encoded spellings (`%2e`, `.%2e`, `%2e.`, `%2e%2e`), which
/// WHATWG URL parsing (browsers) resolves the same way.
pub(super) fn is_dot_segment(segment: &str) -> bool {
    matches!(
        segment.to_ascii_lowercase().as_str(),
        "." | ".." | "%2e" | ".%2e" | "%2e." | "%2e%2e"
    )
}

/// Whether a client would request `path` under another path than the one
/// written: it has a dot segment (see [`is_dot_segment`]) or a backslash, which
/// special schemes such as `http(s)` read as `/`.
pub(super) fn resolved_by_clients(path: &str) -> bool {
    path.contains('\\') || path.split('/').any(is_dot_segment)
}

/// Whether `path` is an RFC 3986 path of ASCII `pchar`s (unreserved,
/// sub-delims, `:` and `@`) and `/` separators, with `%XX` escapes allowed.
/// A request-target is ASCII: a raw non-ASCII character would never match
/// what a client sends (its percent-encoded bytes), so such a route is
/// unreachable.
pub(super) fn is_ascii_path(path: &str) -> bool {
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        let allowed = match byte {
            b'%' => (0..2).all(|_| bytes.next().is_some_and(|hex| hex.is_ascii_hexdigit())),
            _ => byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/".contains(&byte),
        };
        if !allowed {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::dot(".")]
    #[case::dot_dot("..")]
    #[case::upper_encoded("%2E")]
    #[case::mixed_encoded_first(".%2e")]
    #[case::mixed_encoded_second("%2e.")]
    #[case::double_encoded("%2E%2e")]
    fn dot_segments_are_recognised(#[case] segment: &str) {
        assert!(is_dot_segment(segment), "{segment}");
    }

    #[rstest]
    #[case::name("api")]
    #[case::dots_inside("a.b")]
    #[case::triple("...")]
    #[case::encoded_other("%2f")]
    #[case::empty("")]
    fn other_segments_are_not_dot_segments(#[case] segment: &str) {
        assert!(!is_dot_segment(segment), "{segment}");
    }

    #[rstest]
    #[case::plain("/api/docs")]
    #[case::dot_inside_name("/api/v1.2/docs")]
    fn plain_paths_are_not_resolved_by_clients(#[case] path: &str) {
        assert!(!resolved_by_clients(path), "{path}");
    }

    #[rstest]
    #[case::plain("/api/docs-v1.2_~x")]
    #[case::sub_delims_and_at("/a!$&'()*+,;=:@b")]
    #[case::escapes("/caf%C3%A9/%2e")]
    fn ascii_path_characters_are_accepted(#[case] path: &str) {
        assert!(is_ascii_path(path), "{path}");
    }

    #[rstest]
    #[case::non_ascii("/카탈로그")]
    #[case::space("/a b")]
    #[case::bad_escape("/a%zz")]
    #[case::truncated_escape("/a%2")]
    #[case::bracket("/a[b]")]
    fn non_ascii_or_malformed_paths_are_rejected(#[case] path: &str) {
        assert!(!is_ascii_path(path), "{path}");
    }

    #[rstest]
    #[case::dot_segment("/api/../docs")]
    #[case::encoded_dot_segment("/api/%2e%2E/docs")]
    #[case::backslash("/api\\docs")]
    #[case::backslash_dots("/api\\..\\docs")]
    fn resolved_paths_are_rejected(#[case] path: &str) {
        assert!(resolved_by_clients(path), "{path}");
    }
}
