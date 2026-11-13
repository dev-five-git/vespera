//! C ABI ingress cap: an oversized request answers `413` on every entry
//! point before it is copied or dispatched.  Own process: the cap is a
//! process-wide first-wins setting.
#![cfg(feature = "ffi")]
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::sync::mpsc;
use std::time::Duration;

use vespera_inprocess::ffi::{
    VESPERA_OK, vespera_dispatch, vespera_dispatch_async, vespera_dispatch_into, vespera_free,
};

fn status(wire: &[u8]) -> u64 {
    let len = u32::from_be_bytes(wire[..4].try_into().unwrap()) as usize;
    let header: serde_json::Value = serde_json::from_slice(&wire[4..4 + len]).unwrap();
    header["status"].as_u64().unwrap()
}

unsafe extern "C" fn on_response(user_data: *mut c_void, response: *mut u8, len: usize) {
    // SAFETY: boxed sender leaked below; the response is freed once.
    unsafe {
        let sender = Box::from_raw(user_data.cast::<mpsc::Sender<Vec<u8>>>());
        let _ = sender.send(std::slice::from_raw_parts(response, len).to_vec());
        vespera_free(response, len);
    }
}

#[test]
fn oversized_requests_answer_413_everywhere() {
    assert!(vespera_inprocess::set_max_request_bytes(16));
    let req = [0u8; 64];

    let mut out = std::ptr::null_mut();
    let mut out_len = 0usize;
    let mut buf = vec![0u8; 4096];
    let mut written = 0usize;
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let user_data = Box::into_raw(Box::new(tx)).cast::<c_void>();
    // SAFETY: live, disjoint buffers; the response is freed once; the boxed
    // sender is reclaimed by the (inline) callback.
    unsafe {
        assert_eq!(
            vespera_dispatch(req.as_ptr(), req.len(), &raw mut out, &raw mut out_len),
            VESPERA_OK
        );
        assert_eq!(status(std::slice::from_raw_parts(out, out_len)), 413);
        vespera_free(out, out_len);
        assert_eq!(
            vespera_dispatch_into(
                req.as_ptr(),
                req.len(),
                buf.as_mut_ptr(),
                buf.len(),
                &raw mut written
            ),
            VESPERA_OK
        );
        assert_eq!(
            vespera_dispatch_async(req.as_ptr(), req.len(), Some(on_response), user_data),
            VESPERA_OK
        );
    }
    assert_eq!(status(&buf[..written]), 413);
    assert_eq!(
        status(&rx.recv_timeout(Duration::from_secs(5)).unwrap()),
        413
    );
}
