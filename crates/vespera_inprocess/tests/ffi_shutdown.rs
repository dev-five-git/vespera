//! C ABI shutdown: `vespera_shutdown` drains, drops a still-running async
//! dispatch with a `503` callback, and every later call answers `503` /
//! `VESPERA_ERR_RUNTIME`.  Own process: shutdown is irreversible.
#![cfg(feature = "ffi")]
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::sync::mpsc;
use std::time::Duration;

use axum::Router;
use axum::routing::get;
use vespera_inprocess::ffi::{
    ASYNC_DROPPED_MSG, VESPERA_ERR_RUNTIME, VESPERA_OK, vespera_dispatch, vespera_dispatch_async,
    vespera_dispatch_into, vespera_free, vespera_shutdown,
};
use vespera_inprocess::runtime::RUNTIME_SHUT_DOWN_MSG;

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_secs(30)).await;
    "late"
}

fn app() -> Router {
    Router::new().route("/slow", get(slow))
}

vespera_inprocess::ffi_app!(app);

fn request(path: &str) -> Vec<u8> {
    let header =
        serde_json::to_vec(&serde_json::json!({"v": 1, "method": "GET", "path": path})).unwrap();
    let mut wire = u32::try_from(header.len()).unwrap().to_be_bytes().to_vec();
    wire.extend_from_slice(&header);
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

#[test]
fn shutdown_drops_pending_work_and_refuses_new_dispatches() {
    assert_eq!(vespera_init(), VESPERA_OK);
    let slow = request("/slow");
    let pending = start_async(&slow);
    std::thread::sleep(Duration::from_millis(100));

    assert_eq!(vespera_shutdown(50), VESPERA_OK);
    let (status, body) = parse(&pending.recv_timeout(Duration::from_secs(10)).unwrap());
    assert_eq!(
        (status, body.as_slice()),
        (503, ASYNC_DROPPED_MSG.as_bytes())
    );

    assert_eq!(vespera_shutdown(0), VESPERA_ERR_RUNTIME);
    assert_eq!(vespera_init(), VESPERA_ERR_RUNTIME);

    let mut out = std::ptr::null_mut();
    let mut out_len = 0usize;
    let mut buf = vec![0u8; 4096];
    let mut written = 0usize;
    // SAFETY: live, disjoint buffers; the response is freed once.
    unsafe {
        assert_eq!(
            vespera_dispatch(slow.as_ptr(), slow.len(), &raw mut out, &raw mut out_len),
            VESPERA_OK
        );
        let wire = std::slice::from_raw_parts(out, out_len).to_vec();
        vespera_free(out, out_len);
        assert_eq!(
            parse(&wire),
            (503, RUNTIME_SHUT_DOWN_MSG.as_bytes().to_vec())
        );
        assert_eq!(
            vespera_dispatch_into(
                slow.as_ptr(),
                slow.len(),
                buf.as_mut_ptr(),
                buf.len(),
                &raw mut written
            ),
            VESPERA_OK
        );
    }
    assert_eq!(parse(&buf[..written]).0, 503);
    let refused = start_async(&slow)
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert_eq!(parse(&refused).0, 503);
}
