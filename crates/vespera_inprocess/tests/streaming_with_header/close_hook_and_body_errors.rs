use super::*;

// ── M3: request-source close hook ────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidirectional_closing_invokes_close_after_full_read() {
    // M3 regression: when the handler reads the request body (which
    // lazily starts the producer), the `request_close` hook fires
    // exactly once after the response is drained. This is what lets the
    // JNI layer close a Java `InputStream` so a producer parked in a
    // blocking read can't hang the dispatch on a stuck upload.
    install_router();
    let wire = encode_wire(
        "POST",
        "/echo",
        HashMap::from([("content-type", "application/octet-stream")]),
        &[],
    );

    let chunks = vec![b"foo".to_vec(), b"bar".to_vec()];
    let chunks_iter = Mutex::new(chunks.into_iter());
    let pull = move || -> RequestChunk {
        chunks_iter
            .lock()
            .unwrap()
            .next()
            .map_or(RequestChunk::End, RequestChunk::Data)
    };

    let body_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let b = Arc::clone(&body_buf);
    let close_calls = Arc::new(AtomicUsize::new(0));
    let cc = Arc::clone(&close_calls);

    let header = dispatch_bidirectional_streaming_closing(
        wire,
        pull,
        move |chunk| {
            b.lock().unwrap().extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        move || {
            cc.fetch_add(1, Ordering::SeqCst);
        },
    )
    .await;

    let (header_json, _) = decode_wire(&header);
    assert_eq!(header_json["status"].as_u64(), Some(200));
    assert_eq!(body_buf.lock().unwrap().as_slice(), b"foobar");
    assert_eq!(
        close_calls.load(Ordering::SeqCst),
        1,
        "request_close must fire exactly once after a full-read dispatch"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidirectional_with_header_closing_invokes_close_after_full_read() {
    install_router();
    let wire = encode_wire(
        "POST",
        "/echo",
        HashMap::from([("content-type", "application/octet-stream")]),
        &[],
    );

    let payload = Mutex::new(Some(b"payload".to_vec()));
    let pull = move || -> RequestChunk {
        payload
            .lock()
            .unwrap()
            .take()
            .map_or(RequestChunk::End, RequestChunk::Data)
    };

    let header_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let body_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&header_buf);
    let b = Arc::clone(&body_buf);
    let close_calls = Arc::new(AtomicUsize::new(0));
    let cc = Arc::clone(&close_calls);

    let _ = dispatch_bidirectional_streaming_with_header_closing(
        wire,
        pull,
        move |chunk| {
            b.lock().unwrap().extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        move |hdr| h.lock().unwrap().extend_from_slice(hdr),
        move || {
            cc.fetch_add(1, Ordering::SeqCst);
        },
    )
    .await;

    let (header_json, _) = decode_wire(&header_buf.lock().unwrap());
    assert_eq!(header_json["status"].as_u64(), Some(200));
    assert_eq!(body_buf.lock().unwrap().as_slice(), b"payload");
    assert_eq!(
        close_calls.load(Ordering::SeqCst),
        1,
        "request_close must fire exactly once after a full-read dispatch"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidirectional_with_header_closing_skips_close_when_body_ignored() {
    // When the handler never reads the request body, the producer is
    // never started, so there is nothing to close — `request_close` must
    // NOT fire. A GET handler with no body extractor never polls the
    // request body.
    install_router();
    let wire = encode_wire("GET", "/ping", HashMap::new(), &[]);

    let pull = || -> RequestChunk { RequestChunk::End };
    let header_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let body_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&header_buf);
    let b = Arc::clone(&body_buf);
    let close_calls = Arc::new(AtomicUsize::new(0));
    let cc = Arc::clone(&close_calls);

    let _ = dispatch_bidirectional_streaming_with_header_closing(
        wire,
        pull,
        move |chunk| {
            b.lock().unwrap().extend_from_slice(chunk);
            ControlFlow::Continue(())
        },
        move |hdr| h.lock().unwrap().extend_from_slice(hdr),
        move || {
            cc.fetch_add(1, Ordering::SeqCst);
        },
    )
    .await;

    let (header_json, _) = decode_wire(&header_buf.lock().unwrap());
    assert_eq!(header_json["status"].as_u64(), Some(200));
    assert_eq!(
        close_calls.load(Ordering::SeqCst),
        0,
        "request_close must NOT fire when the handler ignores the body (producer never started)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidirectional_closing_invokes_close_on_handler_panic() {
    // Panic-path sibling of M3: the handler reads the full body (starting the
    // producer) and then panics, so the unwind skips the explicit close. The
    // RAII guard in bidirectional_streaming_inner must STILL fire request_close
    // so a producer parked in a blocking source read can be unblocked instead
    // of leaking forever.
    install_router();
    let wire = encode_wire(
        "POST",
        "/read-panic",
        HashMap::from([("content-type", "application/octet-stream")]),
        &[],
    );

    let payload = Mutex::new(Some(b"body".to_vec()));
    let pull = move || -> RequestChunk {
        payload
            .lock()
            .unwrap()
            .take()
            .map_or(RequestChunk::End, RequestChunk::Data)
    };

    let close_calls = Arc::new(AtomicUsize::new(0));
    let cc = Arc::clone(&close_calls);

    // Run on a spawned task so the handler panic surfaces as a JoinError
    // instead of unwinding the test thread.
    let join = tokio::spawn(async move {
        let _ = dispatch_bidirectional_streaming_with_header_closing(
            wire,
            pull,
            |_chunk: &[u8]| ControlFlow::Continue(()),
            |_hdr: &[u8]| {},
            move || {
                cc.fetch_add(1, Ordering::SeqCst);
            },
        )
        .await;
    })
    .await;

    assert!(
        join.is_err(),
        "handler panic must propagate (inprocess does not catch it)"
    );
    assert_eq!(
        close_calls.load(Ordering::SeqCst),
        1,
        "request_close must fire via the drop guard even when the handler panics after starting the producer"
    );
}

// ── Response body stream errors must not be reported as success ───────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn response_streaming_body_error_yields_500_not_truncated_success() {
    // A handler whose response body errors mid-stream must surface a 500
    // through the returned wire header, not the original 200 with a silently
    // truncated body (dispatch_response_streaming path).
    install_router();
    let wire = encode_wire("GET", "/err-body", HashMap::new(), &[]);
    let body_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let b = Arc::clone(&body_buf);

    let header = dispatch_streaming_async(wire, move |chunk| {
        b.lock().unwrap().extend_from_slice(chunk);
        ControlFlow::Continue(())
    })
    .await;

    let (header_json, err_body) = decode_wire(&header);
    assert_eq!(
        header_json["status"].as_u64(),
        Some(500),
        "a response body that errors mid-stream must yield 500, not a truncated 200"
    );
    assert!(
        !err_body.is_empty(),
        "the 500 wire must carry an error body"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streaming_with_header_body_error_returns_body_error_outcome() {
    // Header-first path: the 200 header is committed via `on_header` BEFORE
    // the body drains, so a mid-stream body error can no longer change the
    // status. The dispatch must report `StreamOutcome::BodyError` so the host
    // (JNI bridge) can abort the transport instead of finishing cleanly over a
    // truncated body. Regression guard for the silently-swallowed `Err(_)`.
    install_router();
    let wire = encode_wire("GET", "/err-body", HashMap::new(), &[]);
    let header_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::clone(&header_buf);

    let outcome = dispatch_streaming_with_header_async(
        wire,
        move |header| h.lock().unwrap().extend_from_slice(header),
        |_chunk| ControlFlow::Continue(()),
    )
    .await;

    assert_eq!(
        outcome,
        StreamOutcome::BodyError,
        "a response body that errors after the header is committed must report BodyError"
    );
    // The header committed as 200 — the error only surfaced afterwards.
    let (header_json, _) = decode_wire(&header_buf.lock().unwrap());
    assert_eq!(header_json["status"].as_u64(), Some(200));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streaming_with_header_chunk_break_returns_sink_stopped_outcome() {
    // When the chunk sink returns `Break` (host output sink failed), the
    // header-first path must report `StreamOutcome::SinkStopped` rather than a
    // clean completion, so the JNI bridge can surface the truncation.
    install_router();
    let wire = encode_wire("GET", "/multi-chunk", HashMap::new(), &[]);
    let outcome =
        dispatch_streaming_with_header_async(wire, |_header| {}, |_chunk| ControlFlow::Break(()))
            .await;
    assert_eq!(
        outcome,
        StreamOutcome::SinkStopped,
        "a chunk sink that breaks must report SinkStopped"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn response_streaming_chunk_break_returns_500_not_silent_success() {
    // When the chunk sink returns `Break` (the host output sink failed
    // mid-stream), the non-header `dispatch_streaming_async` must surface a
    // 500 — NOT the original success header — so a TRUNCATED response is never
    // reported as a clean success.  (Mirrors the header-first
    // `...sink_stopped_outcome` and direct-write
    // `...body_error_yields_500_not_truncated_success` contracts.)
    install_router();
    let wire = encode_wire("GET", "/multi-chunk", HashMap::new(), &[]);
    let body_buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let b = Arc::clone(&body_buf);

    let header = dispatch_streaming_async(wire, move |chunk| {
        b.lock().unwrap().extend_from_slice(chunk);
        ControlFlow::Break(())
    })
    .await;

    let (header_json, _header_body) = decode_wire(&header);
    assert_eq!(
        header_json["status"].as_u64(),
        Some(500),
        "a chunk-sink break must yield 500, not a truncated 200 success"
    );
    // The first chunk was already delivered to the sink before the break fired.
    assert_eq!(body_buf.lock().unwrap().as_slice(), b"first");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidirectional_chunk_break_returns_500_not_silent_success() {
    // The non-header BIDIRECTIONAL path must also surface a 500 when the chunk
    // sink breaks mid-response, instead of returning the captured success
    // header (which would report a truncated bidirectional response as a clean
    // success).  Mirrors `response_streaming_chunk_break_returns_500...`.
    install_router();
    let wire = encode_wire("GET", "/multi-chunk", HashMap::new(), &[]);
    let header = dispatch_bidirectional_streaming_closing(
        wire,
        || RequestChunk::End,            // no request body
        |_chunk| ControlFlow::Break(()), // sink fails on the first chunk
        || {},                           // no-op request-source close
    )
    .await;
    let (header_json, _) = decode_wire(&header);
    assert_eq!(
        header_json["status"].as_u64(),
        Some(500),
        "a bidirectional chunk-sink break must yield 500, not truncated success"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn direct_write_body_error_yields_500_not_truncated_success() {
    // Direct-write path: the response is buffered into the caller's slice and
    // only returned at the end, so a body error must rewrite the buffer to a
    // 500 error wire rather than returning the partially-written 200 bytes.
    install_router();
    let wire = encode_wire("GET", "/err-body", HashMap::new(), &[]);
    let mut out = vec![0u8; 4096];

    let result = dispatch_into_async(wire, &mut out).await;
    let n = match result {
        DirectWriteResult::Complete(n) => n,
        DirectWriteResult::Overflow(required) => {
            panic!("expected Complete (500 fits in 4096), got Overflow({required})")
        }
    };

    let (header_json, _) = decode_wire(&out[..n]);
    assert_eq!(
        header_json["status"].as_u64(),
        Some(500),
        "direct-write must emit 500 on a body error, not truncated bytes"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bidirectional_body_error_replaces_captured_success_header_with_500() {
    install_router();
    let wire = encode_wire("GET", "/err-body", HashMap::new(), &[]);

    let header = dispatch_bidirectional_streaming_closing(
        wire,
        || RequestChunk::End,
        |_chunk| ControlFlow::Continue(()),
        || {},
    )
    .await;

    let (header_json, body) = decode_wire(&header);
    assert_eq!(header_json["status"].as_u64(), Some(500));
    assert!(
        !body.is_empty(),
        "replacement 500 wire carries an error body"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bidirectional_rejects_body_bytes_embedded_after_header() {
    install_router();
    let wire = encode_wire("POST", "/echo", HashMap::new(), b"embedded");
    let header_buf = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&header_buf);

    let outcome = dispatch_bidirectional_streaming_with_header(
        wire,
        || RequestChunk::End,
        |_chunk| ControlFlow::Continue(()),
        move |header| captured.lock().unwrap().extend_from_slice(header),
    )
    .await;

    assert_eq!(outcome, StreamOutcome::Complete);
    let (header_json, body) = decode_wire(&header_buf.lock().unwrap());
    assert_eq!(header_json["status"].as_u64(), Some(400));
    assert!(
        String::from_utf8_lossy(&body).contains("header-only"),
        "error explains that request bytes must come from pull_chunk"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn panicking_request_producer_aborts_the_upload() {
    install_router();
    let wire = encode_wire(
        "POST",
        "/echo",
        HashMap::from([("content-type", "application/octet-stream")]),
        &[],
    );

    let header = dispatch_bidirectional_streaming_closing(
        wire,
        || -> RequestChunk { panic!("producer failed") },
        |_chunk| ControlFlow::Continue(()),
        || {},
    )
    .await;

    let (header_json, _) = decode_wire(&header);
    assert_eq!(
        header_json["status"].as_u64(),
        Some(400),
        "a producer panic must become a request-body error, not clean EOF"
    );
}
