//! Request header-name parsing with an interned table of common
//! non-standard names.
//!
//! `HeaderName::from_bytes` allocates (a `Bytes` copy) for every name that is
//! not one of `http`'s standard headers — and browsers / proxies send several
//! per request (`sec-fetch-*`, `sec-ch-ua*`, `x-forwarded-*`, `traceparent`,
//! ...).  [`parse_header_name`] maps the common ones to
//! `HeaderName::from_static` (no allocation, no refcount on clone) and hands
//! everything else to `from_bytes` unchanged: same name, same errors.

use http::HeaderName;
use http::header::InvalidHeaderName;

/// The interned names: exact lowercase spellings only (any other spelling,
/// e.g. `X-Request-Id`, takes `from_bytes`, which lowercases it).  None is a
/// standard `http` header (those never allocate anyway).
const INTERNED: [&str; 22] = [
    "baggage",
    "priority",
    "sec-ch-ua",
    "sec-ch-ua-mobile",
    "sec-ch-ua-platform",
    "sec-fetch-dest",
    "sec-fetch-mode",
    "sec-fetch-site",
    "sec-fetch-user",
    "traceparent",
    "tracestate",
    "x-amzn-trace-id",
    "x-correlation-id",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-port",
    "x-forwarded-prefix",
    "x-forwarded-proto",
    "x-real-ip",
    "x-request-id",
    "x-requested-with",
    "x-b3-traceid",
];

/// Parse a wire request-header name: an interned common name without
/// allocating, anything else exactly as `HeaderName::from_bytes`.
#[inline]
pub fn parse_header_name(name: &str) -> Result<HeaderName, InvalidHeaderName> {
    // Dispatch on the first byte so a standard name (`accept`, `host`, ...)
    // pays at most a few length compares before `from_bytes`.
    let interned = match name.as_bytes().first() {
        Some(b'b' | b'p' | b's' | b't' | b'x') => INTERNED.iter().find(|&&known| known == name),
        _ => None,
    };
    interned.map_or_else(
        || HeaderName::from_bytes(name.as_bytes()),
        |&known| Ok(HeaderName::from_static(known)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interned_names_equal_their_parsed_form() {
        for name in INTERNED {
            let interned = parse_header_name(name).unwrap();
            assert_eq!(interned, HeaderName::from_bytes(name.as_bytes()).unwrap());
            assert_eq!(interned.as_str(), name);
        }
    }

    #[test]
    fn other_names_take_from_bytes_unchanged() {
        for name in [
            "accept",
            "X-Request-Id",
            "x-custom",
            "set-cookie",
            "",
            "bad name",
        ] {
            assert_eq!(
                parse_header_name(name).ok(),
                HeaderName::from_bytes(name.as_bytes()).ok(),
                "{name}"
            );
        }
        assert_eq!(
            parse_header_name("X-Request-Id").unwrap().as_str(),
            "x-request-id"
        );
        assert!(parse_header_name("bad name").is_err());
    }
}
