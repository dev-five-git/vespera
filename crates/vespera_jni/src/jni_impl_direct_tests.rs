use super::write_response_to_out;

#[test]
fn response_fits_returns_len_and_writes_bytes() {
    let mut out = vec![0u8; 16];
    let response = b"hello wire";
    let n = write_response_to_out(out.as_mut_ptr(), out.len(), response);
    assert_eq!(n, 10);
    assert_eq!(&out[..10], response);
}

#[test]
fn exact_fit_boundary() {
    let mut out = vec![0u8; 4];
    let n = write_response_to_out(out.as_mut_ptr(), out.len(), b"abcd");
    assert_eq!(n, 4);
    assert_eq!(&out[..], b"abcd");
}

#[test]
fn overflow_returns_negative_required_size_and_writes_nothing() {
    let mut out = vec![0xAAu8; 4];
    let n = write_response_to_out(out.as_mut_ptr(), out.len(), b"too large");
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
    let n = write_response_to_out(out.as_mut_ptr(), 0, b"x");
    assert_eq!(n, -1);
}
