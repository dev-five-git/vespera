use jni::sys::jint;

use super::{
    DIRECT_UNREPRESENTABLE, direct_complete_code, direct_overflow_code, write_response_to_out,
};

// SAFETY (all tests below): each `out` is a live, writable `Vec<u8>`; the
// `(out.as_mut_ptr(), out.len())` pair describes exactly its allocation, and
// the `response` literal is a distinct Rust-owned slice that cannot alias it —
// satisfying `write_response_to_out`'s `# Safety` contract.

#[test]
fn response_fits_returns_len_and_writes_bytes() {
    let mut out = vec![0u8; 16];
    let response = b"hello wire";
    let n = unsafe { write_response_to_out(out.as_mut_ptr(), out.len(), response) };
    assert_eq!(n, 10);
    assert_eq!(&out[..10], response);
}

#[test]
fn exact_fit_boundary() {
    let mut out = vec![0u8; 4];
    let n = unsafe { write_response_to_out(out.as_mut_ptr(), out.len(), b"abcd") };
    assert_eq!(n, 4);
    assert_eq!(&out[..], b"abcd");
}

#[test]
fn overflow_returns_negative_required_size_and_writes_nothing() {
    let mut out = vec![0xAAu8; 4];
    let n = unsafe { write_response_to_out(out.as_mut_ptr(), out.len(), b"too large") };
    assert_eq!(n, -9);
    assert_eq!(
        &out[..],
        &[0xAA; 4],
        "overflow must not touch the out buffer"
    );
}

#[test]
fn zero_capacity_overflow() {
    let mut out: Vec<u8> = Vec::new();
    let n = unsafe { write_response_to_out(out.as_mut_ptr(), 0, b"x") };
    assert_eq!(n, -1);
}

#[test]
fn complete_code_encodes_written_length() {
    assert_eq!(direct_complete_code(0), 0);
    assert_eq!(direct_complete_code(10), 10);
    let max = usize::try_from(jint::MAX).expect("jint::MAX fits usize");
    assert_eq!(direct_complete_code(max), jint::MAX);
}

#[test]
fn overflow_code_encodes_negated_required_size() {
    assert_eq!(direct_overflow_code(1), -1);
    assert_eq!(direct_overflow_code(9), -9);
    let max = usize::try_from(jint::MAX).expect("jint::MAX fits usize");
    assert_eq!(direct_overflow_code(max), -jint::MAX);
    assert!(
        direct_overflow_code(max) > DIRECT_UNREPRESENTABLE,
        "the most negative legitimate code must stay distinct from the sentinel"
    );
}

#[test]
fn sizes_above_jint_max_collapse_to_the_sentinel() {
    let Some(too_big) = usize::try_from(jint::MAX)
        .ok()
        .and_then(|m| m.checked_add(1))
    else {
        return; // usize is jint-sized; no unrepresentable value exists.
    };
    assert_eq!(direct_complete_code(too_big), DIRECT_UNREPRESENTABLE);
    assert_eq!(direct_overflow_code(too_big), DIRECT_UNREPRESENTABLE);
}
