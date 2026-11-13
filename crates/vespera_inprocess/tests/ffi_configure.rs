//! C ABI configuration: every `vespera_configure_*` applies once and then
//! reports `VESPERA_ERR_TOO_LATE`, and the configured async in-flight cap
//! (shared with JNI `dispatchAsync`) answers a `503` inline once reached.
//! Own process: every knob is a process-wide first-wins setting.
#![cfg(feature = "ffi")]
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::routing::{get, post};
use vespera_inprocess::ffi::{
    VESPERA_ERR_TOO_LATE, VESPERA_OK, vespera_configure_async_max_in_flight,
    vespera_configure_dispatch_timeout_ms, vespera_configure_max_blocking_threads,
    vespera_configure_max_buffered_response_bytes, vespera_configure_max_request_bytes,
    vespera_configure_worker_threads, vespera_dispatch, vespera_dispatch_async, vespera_free,
};
use vespera_inprocess::runtime::{ASYNC_OVERLOADED_MSG, async_in_flight};

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_millis(500)).await;
    "slow"
}

fn app() -> Router {
    Router::new()
        .route("/slow", get(slow))
        .route("/fast", get(|| async { "fast" }))
        .route("/echo", post(|body: String| async move { body }))
}

vespera_inprocess::ffi_app!(app);

fn request(method: &str, path: &str, body: &[u8]) -> Vec<u8> {
    let header =
        serde_json::to_vec(&serde_json::json!({"v": 1, "method": method, "path": path})).unwrap();
    let mut wire = u32::try_from(header.len()).unwrap().to_be_bytes().to_vec();
    wire.extend_from_slice(&header);
    wire.extend_from_slice(body);
    wire
}

fn parse(wire: &[u8]) -> (u64, Vec<u8>) {
    let len = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
    let header: serde_json::Value = serde_json::from_slice(&wire[4..4 + len]).unwrap();
    (header["status"].as_u64().unwrap(), wire[4 + len..].to_vec())
}

unsafe extern "C" fn on_response(user_data: *mut c_void, response: *mut u8, len: usize) {
    // SAFETY: boxed sender leaked by `start_async`; the response is freed once.
    unsafe {
        let sender = Box::from_raw(user_data.cast::<mpsc::Sender<Vec<u8>>>());
        let _ = sender.send(std::slice::from_raw_parts(response, len).to_vec());
        vespera_free(response, len);
    }
}

fn start_async(req: &[u8]) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    let user_data = Box::into_raw(Box::new(tx)).cast::<c_void>();
    // SAFETY: live request; the boxed sender is reclaimed by the callback.
    let code =
        unsafe { vespera_dispatch_async(req.as_ptr(), req.len(), Some(on_response), user_data) };
    assert_eq!(code, VESPERA_OK);
    rx
}

fn recv(rx: &mpsc::Receiver<Vec<u8>>) -> (u64, Vec<u8>) {
    parse(&rx.recv_timeout(Duration::from_secs(10)).unwrap())
}

fn dispatch(req: &[u8]) -> (u64, Vec<u8>) {
    let (mut out, mut out_len) = (std::ptr::null_mut(), 0usize);
    // SAFETY: live request; the response is copied, then freed once.
    unsafe {
        assert_eq!(
            vespera_dispatch(req.as_ptr(), req.len(), &raw mut out, &raw mut out_len),
            VESPERA_OK
        );
        let wire = std::slice::from_raw_parts(out, out_len).to_vec();
        vespera_free(out, out_len);
        parse(&wire)
    }
}

#[test]
fn configure_applies_once_and_caps_async_dispatches() {
    let knobs: [&dyn Fn() -> i32; 6] = [
        &|| vespera_configure_worker_threads(2),
        &|| vespera_configure_max_blocking_threads(16),
        &|| vespera_configure_max_request_bytes(1024),
        &|| vespera_configure_max_buffered_response_bytes(1 << 20),
        &|| vespera_configure_dispatch_timeout_ms(10_000),
        &|| vespera_configure_async_max_in_flight(1),
    ];
    for knob in knobs {
        assert_eq!(knob(), VESPERA_OK);
        assert_eq!(knob(), VESPERA_ERR_TOO_LATE, "first-wins");
    }
    assert_eq!(vespera_init(), VESPERA_OK);

    // The single in-flight slot is taken: the next async dispatch is
    // answered inline with a 503 and never reaches the handler.
    let pending = start_async(&request("GET", "/slow", b""));
    assert_eq!(async_in_flight(), 1);
    let (status, body) = recv(&start_async(&request("GET", "/fast", b"")));
    assert_eq!(
        (status, body.as_slice()),
        (503, ASYNC_OVERLOADED_MSG.as_bytes())
    );
    assert_eq!(recv(&pending), (200, b"slow".to_vec()));

    // The slot is released once the completed task drops its permit.
    let deadline = Instant::now() + Duration::from_secs(10);
    while async_in_flight() != 0 {
        assert!(Instant::now() < deadline, "permit never released");
        std::thread::yield_now();
    }
    assert_eq!(
        recv(&start_async(&request("GET", "/fast", b""))),
        (200, b"fast".to_vec())
    );

    // The configured request cap applies to the C ABI too.
    assert_eq!(dispatch(&request("POST", "/echo", &[b'x'; 2048])).0, 413);
    assert_eq!(
        dispatch(&request("POST", "/echo", b"hi")),
        (200, b"hi".to_vec())
    );
}
