//! C ABI shutdown: an async dispatch dropped by `vespera_shutdown` releases
//! its in-flight slot BEFORE its `503` callback runs — the same order as a
//! normal completion — so a callback that dispatches again under a cap of 1
//! is answered "runtime has been shut down", never "too many in-flight".
//! Own process: shutdown is irreversible and the cap is process-wide.
#![cfg(feature = "ffi")]
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::sync::mpsc;
use std::time::Duration;

use axum::Router;
use axum::routing::get;
use vespera_inprocess::ffi::{
    ASYNC_DROPPED_MSG, VESPERA_OK, vespera_configure_async_max_in_flight, vespera_dispatch_async,
    vespera_free, vespera_shutdown,
};
use vespera_inprocess::runtime::{RUNTIME_SHUT_DOWN_MSG, async_in_flight};

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_secs(30)).await;
    "late"
}

fn app() -> Router {
    Router::new().route("/slow", get(slow))
}

vespera_inprocess::ffi_app!(app);

fn request() -> Vec<u8> {
    let header =
        serde_json::to_vec(&serde_json::json!({"v": 1, "method": "GET", "path": "/slow"})).unwrap();
    let mut wire = u32::try_from(header.len()).unwrap().to_be_bytes().to_vec();
    wire.extend_from_slice(&header);
    wire
}

fn body(wire: &[u8]) -> String {
    let len = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
    String::from_utf8(wire[4 + len..].to_vec()).unwrap()
}

/// What the first callback saw: its own body, the in-flight count, and the
/// body of the dispatch it chained.
type Seen = (String, usize, String);

unsafe extern "C" fn on_chained(user_data: *mut c_void, response: *mut u8, len: usize) {
    // SAFETY: boxed slot leaked by `on_dropped`; the response is freed once.
    unsafe {
        let slot = Box::from_raw(user_data.cast::<mpsc::Sender<String>>());
        let _ = slot.send(body(std::slice::from_raw_parts(response, len)));
        vespera_free(response, len);
    }
}

// An `extern "C"` callback cannot unwind: a failed assertion here would abort
// the whole test binary instead of failing the test. Every observed value is
// therefore sent through the channel and asserted by the test body.
unsafe extern "C" fn on_dropped(user_data: *mut c_void, response: *mut u8, len: usize) {
    // SAFETY: boxed sender leaked by the test; the response is freed once.
    unsafe {
        let sender = Box::from_raw(user_data.cast::<mpsc::Sender<Seen>>());
        let own = body(std::slice::from_raw_parts(response, len));
        vespera_free(response, len);
        let in_flight = async_in_flight();
        let (tx, rx) = mpsc::channel::<String>();
        let req = request();
        let chained_data = Box::into_raw(Box::new(tx)).cast::<c_void>();
        let code = vespera_dispatch_async(req.as_ptr(), req.len(), Some(on_chained), chained_data);
        let chained = if code == VESPERA_OK {
            rx.recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|error| format!("no chained answer: {error}"))
        } else {
            format!("chained dispatch refused with code {code}")
        };
        let _ = sender.send((own, in_flight, chained));
    }
}

#[test]
fn a_dropped_async_dispatch_frees_its_slot_before_the_callback() {
    assert_eq!(vespera_configure_async_max_in_flight(1), VESPERA_OK);
    assert_eq!(vespera_init(), VESPERA_OK);
    let (tx, rx) = mpsc::channel::<Seen>();
    let req = request();
    let user_data = Box::into_raw(Box::new(tx)).cast::<c_void>();
    // SAFETY: live request; the boxed sender is reclaimed by the callback.
    let code =
        unsafe { vespera_dispatch_async(req.as_ptr(), req.len(), Some(on_dropped), user_data) };
    assert_eq!(code, VESPERA_OK);
    std::thread::sleep(Duration::from_millis(100));

    assert_eq!(vespera_shutdown(50), VESPERA_OK);
    let (own, in_flight, chained) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(own, ASYNC_DROPPED_MSG);
    assert_eq!(in_flight, 0, "the slot is released before the callback");
    assert_eq!(chained, RUNTIME_SHUT_DOWN_MSG);
}
