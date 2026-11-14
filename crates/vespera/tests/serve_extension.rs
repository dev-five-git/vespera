//! End-to-end test for the [`vespera::Serve`] extension trait that
//! exposes the `axum::Router::serve(addr)` one-liner.
//!
//! Every server runs on a listener the test binds FIRST (port `0`) and hands
//! over with `serve_listener*`: the address is known before the server task
//! starts and a client may connect at once (the listen backlog queues it), so
//! there is neither a bind-drop-rebind port race nor a "wait for the server"
//! sleep. Requests go over a raw `TcpStream` (no HTTP client dependency).
//! The address-taking `serve*` methods are covered by their bind-failure
//! path on an occupied port.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use axum::Router;
use axum::extract::ConnectInfo;
use axum::routing::get;
use vespera::{Serve, VesperaRouter};

/// Bind an ephemeral localhost port; the server is handed this very socket.
async fn bind() -> (tokio::net::TcpListener, SocketAddr) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    (listener, addr)
}

/// Send a raw `GET path` and return the whole response text.
fn raw_get(addr: SocketAddr, path: &str) -> String {
    let mut stream =
        TcpStream::connect_timeout(&addr, Duration::from_secs(2)).expect("connect to server");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set_read_timeout");
    let request = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).expect("write request");
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

async fn get_async(addr: SocketAddr, path: &'static str) -> String {
    tokio::task::spawn_blocking(move || raw_get(addr, path))
        .await
        .expect("blocking task")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_listener_handles_requests() {
    let (listener, addr) = bind().await;
    let app = Router::new().route("/ping", get(|| async { "pong" }));
    // `serve_listener` runs until the task is aborted.
    let handle = tokio::spawn(app.serve_listener(listener));

    let response = get_async(addr, "/ping").await;
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "expected 200 OK, got: {response:?}"
    );
    assert!(response.ends_with("pong"), "body 'pong': {response:?}");

    handle.abort();
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn merged_stateless_serve_listener_finalizes_before_serving() {
    let (listener, addr) = bind().await;
    let app = VesperaRouter::new(
        Router::<()>::new().route("/merged", get(|| async { "finalized" })),
        Vec::new(),
    );
    let handle = tokio::spawn(app.serve_listener(listener));

    let response = get_async(addr, "/merged").await;
    assert!(
        response.starts_with("HTTP/1.1 200") && response.ends_with("finalized"),
        "merged router serves its routes: {response:?}"
    );

    handle.abort();
    let _ = handle.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_returns_error_when_port_is_in_use() {
    // Hold a listener on an ephemeral port so the bind inside
    // `Serve::serve` fails with AddrInUse.
    let hog = TcpListener::bind("127.0.0.1:0").expect("bind hog");
    let addr: SocketAddr = hog.local_addr().expect("local_addr");

    let plain = Router::new().serve(addr).await;
    assert_eq!(
        plain.expect_err("occupied port").kind(),
        std::io::ErrorKind::AddrInUse
    );

    let merged = VesperaRouter::new(Router::<()>::new(), Vec::new())
        .serve(addr)
        .await;
    assert_eq!(
        merged.expect_err("occupied port").kind(),
        std::io::ErrorKind::AddrInUse
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graceful_serve_listener_exposes_connect_info_and_stops_on_signal() {
    let (listener, addr) = bind().await;
    let app = Router::new().route(
        "/peer",
        get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move { peer.ip().to_string() }),
    );
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(
        app.serve_listener_with_graceful_shutdown(listener, async move {
            let _ = stopped.await;
        }),
    );

    let response = get_async(addr, "/peer").await;
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "ConnectInfo extractor must be served: {response:?}"
    );
    assert!(
        response.ends_with("127.0.0.1"),
        "peer ip body: {response:?}"
    );

    // The shutdown signal fires → the server future completes cleanly.
    stop.send(()).expect("server still running");
    let result = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("server stops after the signal")
        .expect("server task");
    assert!(result.is_ok(), "graceful shutdown returns Ok: {result:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn merged_graceful_serve_listener_stops_on_signal() {
    let (listener, addr) = bind().await;
    let app = VesperaRouter::new(
        Router::<()>::new().route("/merged", get(|| async { "finalized" })),
        Vec::new(),
    );
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(
        app.serve_listener_with_graceful_shutdown(listener, async move {
            let _ = stopped.await;
        }),
    );

    let response = get_async(addr, "/merged").await;
    assert!(response.ends_with("finalized"), "{response:?}");

    stop.send(()).expect("server still running");
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
