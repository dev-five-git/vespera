use super::*;

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime")
        .block_on(fut)
}

#[test]
fn non_ascii_header_value_maps_latin1_chars_back_to_bytes() {
    let owner = Bytes::from_static("r\u{c3}\u{a9}sum\u{c3}\u{a9}".as_bytes());
    let value = header_value_from_owner("r\u{c3}\u{a9}sum\u{c3}\u{a9}", Some(&owner)).unwrap();
    assert_eq!(value.as_bytes(), "r\u{e9}sum\u{e9}".as_bytes());

    let wide = header_value_from_owner("\u{d55c}\u{ae00}", None).unwrap();
    assert_eq!(wide.as_bytes(), "\u{d55c}\u{ae00}".as_bytes());
}

#[test]
fn non_ascii_response_header_survives_envelope_conversion() {
    let value = http::HeaderValue::from_bytes(b"caf\xe9").unwrap();
    assert_eq!(header_value_to_owned(&value), "caf\u{e9}");
}

#[test]
fn header_value_falls_back_to_copy_when_value_is_outside_owner() {
    let owner = Bytes::from_static(b"wire-owned-value");

    let value = header_value_from_owner("external-value", Some(&owner)).unwrap();

    assert_eq!(value.as_bytes(), b"external-value");
}

/// A wire `path` that cannot be parsed into an [`http::Uri`] (a raw
/// space is illegal) must surface as an `Err((4xx, _))` the caller
/// turns into a wire response — never a panic.  Guards the
/// "all failure modes return a valid wire response" contract for
/// every `request_builder` call site.
#[test]
fn malformed_path_returns_error_not_panic() {
    let result = block_on(async {
        dispatch_parts(
            crate::Router::new(),
            "GET",
            "bad path with spaces",
            "",
            std::iter::empty(),
            Bytes::new(),
            None,
            None,
        )
        .await
    });
    match result {
        Err((status, _)) => assert!(
            (400..500).contains(&status),
            "expected 4xx for malformed path, got {status}"
        ),
        Ok(_) => panic!("malformed path should not produce a successful dispatch"),
    }
}

#[test]
fn malformed_path_split_returns_error_not_panic() {
    let result = block_on(async {
        dispatch_and_split(
            crate::Router::new(),
            "GET",
            "bad path with spaces",
            "",
            None,
            std::iter::empty(),
            None,
            Body::empty(),
            false,
            ConnectionInfo::default(),
        )
        .await
    });
    assert!(
        result.is_err(),
        "dispatch_and_split must reject malformed path"
    );
}

#[rstest::rstest]
#[case::ipv4(
    Some("10.1.2.3:8443"),
    Some("https"),
    Some("10.1.2.3:8443"),
    Some(Scheme::HTTPS)
)]
#[case::ipv6_upper_scheme(Some(" [::1]:80 "), Some("HTTP"), Some("[::1]:80"), Some(Scheme::HTTP))]
#[case::unparseable(Some("fe80::1%eth0"), Some("ftp"), None, None)]
#[case::absent(None, None, None, None)]
fn connection_info_parses_or_ignores_wire_values(
    #[case] remote_addr: Option<&str>,
    #[case] scheme: Option<&str>,
    #[case] expected_addr: Option<&str>,
    #[case] expected_scheme: Option<Scheme>,
) {
    let info = ConnectionInfo::parse(remote_addr, scheme);
    assert_eq!(
        info.remote_addr,
        expected_addr.map(|a| a.parse().expect("valid test addr"))
    );
    assert_eq!(info.scheme, expected_scheme);

    let mut extensions = http::Extensions::new();
    info.clone().apply_to(&mut extensions);
    #[cfg(feature = "connect-info")]
    assert_eq!(
        extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0),
        info.remote_addr
    );
    assert_eq!(extensions.get::<Scheme>(), info.scheme.as_ref());
}

#[rstest::rstest]
#[case::no_content_length(&[("content-type", "text/plain")], false)]
#[case::zero(&[("content-length", "0")], true)]
#[case::zero_padded_mixed_case(&[("Content-Length", " 00\t")], true)]
#[case::non_zero(&[("content-length", "10")], false)]
#[case::blank(&[("content-length", " ")], false)]
#[case::duplicate_zeros(&[("content-length", "0"), ("content-length", "0")], true)]
#[case::zero_then_non_zero(&[("content-length", "0"), ("content-length", "1")], false)]
fn declares_empty_body_needs_every_content_length_zero(
    #[case] headers: &[(&str, &str)],
    #[case] expected: bool,
) {
    assert_eq!(
        declares_empty_body(headers.iter().map(|&(n, v)| (n, v))),
        expected
    );
}

#[test]
fn bench_request_builders_cover_query_headers_and_json_default() {
    let headers = [("x-a", "abc"), ("content-type", "text/plain")];
    let body = Bytes::from_static(b"{}");

    assert_eq!(
        bench_build_request_new("POST", "/items", "x=1", &headers, body.clone()),
        41
    );
    assert_eq!(
        bench_build_request_old("POST", "/items", "x=1", &headers, body.clone()),
        41
    );
    assert_eq!(bench_build_request_old("POST", "/x", "", &[], body), 34);
}

/// Round 44 (IP1-L1): the bench twin honours the `default_json_content_type`
/// decision like production. Passed explicitly: the knob is process-wide.
#[test]
fn bench_twin_gates_the_json_default_on_the_knob() {
    let body = Bytes::from_static(b"{}");
    let len = |on: bool| {
        build_request_from_bytes_builder_old_with(
            "POST",
            "/x",
            "",
            std::iter::empty(),
            body.clone(),
            on,
        )
        .map_or(usize::MAX, |req| request_field_len_sum(&req))
    };
    // Knob on: `content-type: application/json` (28 bytes) is counted.
    assert_eq!(len(true), 34);
    // Knob off: the untyped body is sent without a content-type.
    assert_eq!(len(false), 6);
}

#[test]
fn bench_builders_agree_on_non_ascii_header_bytes() {
    // `caf\u{e9}` is 4 wire bytes (Latin-1), 5 UTF-8 bytes: both twins
    // must count the wire bytes, as the production path does.
    let headers = [("x-n", "caf\u{e9}")];
    assert_eq!(
        bench_build_request_new("GET", "/", "", &headers, Bytes::new()),
        11
    );
    assert_eq!(
        bench_build_request_old("GET", "/", "", &headers, Bytes::new()),
        11
    );
}

#[test]
fn old_bench_builder_maps_invalid_method_header_and_uri_to_sentinel() {
    assert_eq!(
        bench_build_request_old("BAD METHOD", "/", "", &[], Bytes::new()),
        usize::MAX
    );
    assert_eq!(
        bench_build_request_old("GET", "/", "", &[("bad header", "value")], Bytes::new(),),
        usize::MAX
    );
    assert_eq!(
        bench_build_request_old("GET", "bad path", "", &[], Bytes::new()),
        usize::MAX
    );
}
