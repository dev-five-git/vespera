//! End-to-end test for the [`vespera::Serve`] extension trait that
//! exposes the `axum::Router::serve(addr)` one-liner.
//!
//! Drives the real `tokio::net::TcpListener::bind` + `axum::serve`
//! code path on an ephemeral port, then issues an HTTP request via a
//! raw TCP `TcpStream` (no extra HTTP client dependency).  Verifies
//! the server responds successfully and that calling `serve` on an
//! already-bound address surfaces a `std::io::Error`.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use axum::Router;
use axum::extract::ConnectInfo;
use axum::routing::get;
use vespera::{Serve, VesperaRouter};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_binds_and_handles_requests_on_ephemeral_port() {
    // Reserve an ephemeral port by binding+dropping a std listener;
    // tokio's TcpListener::bind below re-binds the same address.
    let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe");
    let addr: SocketAddr = probe.local_addr().expect("local_addr");
    drop(probe);

    let app = Router::new().route("/ping", get(|| async { "pong" }));

    // Spawn the server in the background — Serve::serve loops until
    // the listener stops, so we abort it after the request returns.
    let handle = tokio::spawn(async move { app.serve(addr).await });

    // Wait briefly for the listener to come up.  100 ms is generous —
    // bind+listen is sub-millisecond on localhost.
    tokio::time::sleep(Duration::from_millis(150)).await;

    let response_bytes = tokio::task::spawn_blocking(move || -> Vec<u8> {
        let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))
            .expect("connect to bound server");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set_read_timeout");
        let request = b"GET /ping HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
        stream.write_all(request).expect("write request");
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf);
        buf
    })
    .await
    .expect("blocking task");

    let response = String::from_utf8_lossy(&response_bytes);
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "expected 200 OK, got: {response:?}"
    );
    assert!(
        response.contains("pong"),
        "expected body 'pong' in response: {response:?}"
    );

    // Abort the server task — it would otherwise run forever.
    handle.abort();
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_returns_error_when_port_is_in_use() {
    // Hold a listener on an ephemeral port so the subsequent bind
    // call inside Serve::serve fails with AddrInUse.
    let hog = TcpListener::bind("127.0.0.1:0").expect("bind hog");
    let addr: SocketAddr = hog.local_addr().expect("local_addr");

    let app: Router = Router::new();
    let result = app.serve(addr).await;

    assert!(result.is_err(), "serve should fail on occupied port");
    drop(hog);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn merged_stateless_serve_finalizes_before_binding() {
    let hog = TcpListener::bind("127.0.0.1:0").expect("bind hog");
    let addr: SocketAddr = hog.local_addr().expect("local_addr");
    let app = VesperaRouter::new(Router::<()>::new(), Vec::new());

    let result = app.serve(addr).await;

    let error = result.expect_err("occupied port must reject merged router serve");
    assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
}

/// Send a raw `GET path` and return the whole response text.
fn raw_get(addr: SocketAddr, path: &str) -> String {
    let mut stream =
        TcpStream::connect_timeout(&addr, Duration::from_secs(2)).expect("connect to server");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set_read_timeout");
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).expect("write request");
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graceful_serve_exposes_connect_info_and_stops_on_signal() {
    let probe = TcpListener::bind("127.0.0.1:0").expect("bind probe");
    let addr: SocketAddr = probe.local_addr().expect("local_addr");
    drop(probe);

    let app = Router::new().route(
        "/peer",
        get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move { peer.ip().to_string() }),
    );
    let handle = tokio::spawn(async move {
        app.serve_with_graceful_shutdown(addr, tokio::time::sleep(Duration::from_millis(600)))
            .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    let response = tokio::task::spawn_blocking(move || raw_get(addr, "/peer"))
        .await
        .expect("blocking task");
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "ConnectInfo extractor must be served: {response:?}"
    );
    assert!(
        response.ends_with("127.0.0.1"),
        "peer ip body: {response:?}"
    );

    // The shutdown signal fires → the server future completes cleanly.
    let result = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("server stops after the signal")
        .expect("server task");
    assert!(result.is_ok(), "graceful shutdown returns Ok: {result:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graceful_serve_returns_error_when_port_is_in_use() {
    let hog = TcpListener::bind("127.0.0.1:0").expect("bind hog");
    let addr: SocketAddr = hog.local_addr().expect("local_addr");

    let plain = Router::new().serve_with_graceful_shutdown(addr, async {});
    assert_eq!(
        plain.await.expect_err("occupied port").kind(),
        std::io::ErrorKind::AddrInUse
    );

    let merged = VesperaRouter::new(Router::<()>::new(), Vec::new())
        .serve_with_graceful_shutdown(addr, async {});
    assert_eq!(
        merged.await.expect_err("occupied port").kind(),
        std::io::ErrorKind::AddrInUse
    );
}
