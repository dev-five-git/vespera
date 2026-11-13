//! C ABI (`feature = "ffi"`): the `extern "C"` symbols called directly, the
//! way a C / Go / Python host calls them.  Shutdown lives in `ffi_shutdown.rs`
//! (irreversible, so it needs its own process).
#![cfg(feature = "ffi")]
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::sync::mpsc;
use std::time::Duration;

use axum::Router;
use axum::routing::get;
use serde_json::{Value, json};
use vespera_inprocess::ffi::{
    __init, NULL_REQUEST_MSG, VESPERA_ERR_PANIC, VESPERA_ERR_REGISTRATION, VESPERA_OK,
    VESPERA_OVERFLOW, vespera_dispatch, vespera_dispatch_async, vespera_dispatch_into,
    vespera_free,
};

async fn hello() -> &'static str {
    "hello from rust"
}

async fn admin() -> &'static str {
    "admin"
}

async fn big() -> String {
    "x".repeat(10_000)
}

async fn boom() -> &'static str {
    panic!("ffi test handler panic")
}

fn app() -> Router {
    Router::new()
        .route("/hello", get(hello))
        .route("/big", get(big))
        .route("/boom", get(boom))
}

fn admin_app() -> Router {
    Router::new().route("/who", get(admin))
}

vespera_inprocess::ffi_apps! {
    "_default" => app,
    "admin" => admin_app,
}

fn init() {
    assert_eq!(vespera_init(), VESPERA_OK);
}

fn request(path: &str, app: Option<&str>) -> Vec<u8> {
    let mut header = json!({"v": 1, "method": "GET", "path": path});
    if let Some(app) = app {
        header["app"] = json!(app);
    }
    let header = serde_json::to_vec(&header).unwrap();
    let mut wire = u32::try_from(header.len()).unwrap().to_be_bytes().to_vec();
    wire.extend_from_slice(&header);
    wire
}

/// `(status, header JSON, body)` of a wire response.
fn parse(wire: &[u8]) -> (u64, Value, Vec<u8>) {
    let len = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
    let header: Value = serde_json::from_slice(&wire[4..4 + len]).unwrap();
    (
        header["status"].as_u64().unwrap(),
        header,
        wire[4 + len..].to_vec(),
    )
}

/// `vespera_dispatch`, copying the response out and freeing it.
fn dispatch_raw(req: *const u8, len: usize) -> Vec<u8> {
    let mut out = std::ptr::null_mut();
    let mut out_len = 0usize;
    // SAFETY: `req` is valid for `len` bytes (or NULL, tested explicitly);
    // the response is copied, then freed exactly once.
    unsafe {
        assert_eq!(
            vespera_dispatch(req, len, &raw mut out, &raw mut out_len),
            VESPERA_OK
        );
        let copy = std::slice::from_raw_parts(out, out_len).to_vec();
        vespera_free(out, out_len);
        copy
    }
}

fn dispatch(req: &[u8]) -> (u64, Value, Vec<u8>) {
    parse(&dispatch_raw(req.as_ptr(), req.len()))
}

/// `vespera_dispatch_into` with a `cap`-byte buffer: `(code, written, buffer)`.
fn dispatch_into(req: &[u8], cap: usize) -> (i32, usize, Vec<u8>) {
    let mut buf = vec![0u8; cap];
    let mut written = 0usize;
    // SAFETY: disjoint, live buffers of the stated sizes.
    let code = unsafe {
        vespera_dispatch_into(
            req.as_ptr(),
            req.len(),
            buf.as_mut_ptr(),
            cap,
            &raw mut written,
        )
    };
    (code, written, buf)
}

unsafe extern "C" fn on_response(user_data: *mut c_void, response: *mut u8, len: usize) {
    // SAFETY: `user_data` is the boxed sender leaked by `dispatch_async`;
    // the response is owned by this callback and freed once.
    unsafe {
        let sender = Box::from_raw(user_data.cast::<mpsc::Sender<Vec<u8>>>());
        let _ = sender.send(std::slice::from_raw_parts(response, len).to_vec());
        vespera_free(response, len);
    }
}

fn dispatch_async(req: *const u8, len: usize) -> Vec<u8> {
    let (tx, rx) = mpsc::channel();
    let user_data = Box::into_raw(Box::new(tx)).cast::<c_void>();
    // SAFETY: `req` valid for `len` bytes (or NULL, tested explicitly);
    // `user_data` is reclaimed by `on_response`.
    let code = unsafe { vespera_dispatch_async(req, len, Some(on_response), user_data) };
    assert_eq!(code, VESPERA_OK);
    rx.recv_timeout(Duration::from_secs(10))
        .expect("callback must run exactly once")
}

#[test]
fn init_is_idempotent() {
    init();
    init();
    assert!(vespera_inprocess::runtime::host_loaded());
}

#[test]
fn round_trip_default_and_named_apps() {
    init();
    let (status, _, body) = dispatch(&request("/hello", None));
    assert_eq!((status, body.as_slice()), (200, &b"hello from rust"[..]));
    let (status, _, body) = dispatch(&request("/who", Some("admin")));
    assert_eq!((status, body.as_slice()), (200, &b"admin"[..]));
    assert_eq!(dispatch(&request("/hello", Some("nope"))).0, 404);
}

#[test]
fn malformed_and_null_requests_answer_400() {
    init();
    assert_eq!(dispatch(&[0, 0, 0, 9, b'{']).0, 400);
    assert_eq!(parse(&dispatch_raw(std::ptr::null(), 0)).0, 400);
    let (status, _, body) = parse(&dispatch_raw(std::ptr::null(), 5));
    assert_eq!(
        (status, body.as_slice()),
        (400, NULL_REQUEST_MSG.as_bytes())
    );
}

#[test]
fn handler_panic_answers_500_with_panicked_metadata() {
    init();
    let req = request("/boom", None);
    let (status, header, _) = dispatch(&req);
    assert_eq!(status, 500);
    assert_eq!(header["metadata"]["panicked"], "true");
    let (code, written, buf) = dispatch_into(&req, 4096);
    assert_eq!(code, VESPERA_OK);
    assert_eq!(parse(&buf[..written]).0, 500);
    let (status, header, _) = parse(&dispatch_async(req.as_ptr(), req.len()));
    assert_eq!(status, 500);
    assert_eq!(header["metadata"]["panicked"], "true");
}

#[test]
fn dispatch_into_complete_overflow_and_probe() {
    init();
    let req = request("/hello", None);
    let (code, written, buf) = dispatch_into(&req, 4096);
    assert_eq!(code, VESPERA_OK);
    assert_eq!(parse(&buf[..written]).2, b"hello from rust");

    let big = request("/big", None);
    let (code, required, _) = dispatch_into(&big, 64);
    assert_eq!(code, VESPERA_OVERFLOW);
    assert!(required > 10_000);
    let (code, written, buf) = dispatch_into(&big, required);
    assert_eq!((code, written), (VESPERA_OK, required));
    assert_eq!(parse(&buf[..written]).2.len(), 10_000);

    // Size probe: NULL buffer with zero capacity.
    let mut probed = 0usize;
    // SAFETY: zero-capacity NULL output is part of the contract.
    let code = unsafe {
        vespera_dispatch_into(
            req.as_ptr(),
            req.len(),
            std::ptr::null_mut(),
            0,
            &raw mut probed,
        )
    };
    assert_eq!(code, VESPERA_OVERFLOW);
    assert!(probed > 0);

    let mut buf = vec![0u8; 4096];
    let mut written = 0usize;
    // SAFETY: NULL request with a non-zero length is rejected with a 400 wire.
    let code = unsafe {
        vespera_dispatch_into(
            std::ptr::null(),
            3,
            buf.as_mut_ptr(),
            buf.len(),
            &raw mut written,
        )
    };
    assert_eq!(code, VESPERA_OK);
    assert_eq!(parse(&buf[..written]).0, 400);
}

#[test]
fn async_dispatch_delivers_exactly_once() {
    init();
    let req = request("/hello", None);
    let (status, _, body) = parse(&dispatch_async(req.as_ptr(), req.len()));
    assert_eq!((status, body.as_slice()), (200, &b"hello from rust"[..]));
    let (status, _, body) = parse(&dispatch_async(std::ptr::null(), 2));
    assert_eq!(
        (status, body.as_slice()),
        (400, NULL_REQUEST_MSG.as_bytes())
    );
}

#[test]
fn sync_dispatch_inside_a_runtime_degrades_to_500() {
    init();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let req = request("/hello", None);
    let (wire, into) = rt.block_on(async {
        let wire = dispatch_raw(req.as_ptr(), req.len());
        (wire, dispatch_into(&req, 4096))
    });
    let (status, header, _) = parse(&wire);
    assert_eq!(
        (status, &header["metadata"]["panicked"]),
        (500, &json!("true"))
    );
    assert_eq!(into.0, VESPERA_OK);
    assert_eq!(parse(&into.2[..into.1]).0, 500);
}

#[test]
fn init_reports_registration_failure_and_panics() {
    // `vespera_init` from inside a router factory re-enters the registry.
    let outer = vespera_inprocess::try_register_app_named("ffi_outer", || {
        assert_eq!(vespera_init(), VESPERA_ERR_REGISTRATION);
        Router::new()
    });
    assert_eq!(outer, Ok(true));
    assert_eq!(__init(|| false), VESPERA_ERR_REGISTRATION);
    assert_eq!(__init(|| panic!("factory panic")), VESPERA_ERR_PANIC);
}
