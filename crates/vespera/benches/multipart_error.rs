//! Before/after A/B benchmark for the multipart `4xx`/`422` error-envelope
//! serialization (the cold but attacker-reachable malformed-input path).
//!
//! Both arms serialize the **same** [`TypedMultipartError`] to the **same**
//! bytes (`{"errors":[{"message":...,"path":...}]}`):
//!
//! - `before`: the original implementation — materialize the public message
//!   with `error.to_string()` (one heap `String` per error) and serialize an
//!   owned-`&str` envelope.
//! - `after`: the shipped implementation — a borrowing `Serialize` chain that
//!   streams the error's own `Display` straight into `serde_json` via
//!   `collect_str` (zero per-error `String` allocation), mirroring the
//!   `Validated<T>` 422 serializer in `multipart.rs`.
//!
//! The delta is the per-error `String` allocation the change removes. Both
//! arms assert byte-identical output so the bench can never silently drift
//! from the real envelope contract.

use std::borrow::Cow;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use serde::{Serialize, Serializer, ser::SerializeStruct};
use vespera::multipart::TypedMultipartError;

/// A realistic client-caused multipart error (the common 422 case): a scalar
/// field whose value failed to parse. Its `Display` carries the field name,
/// the wanted type, and the parse error — representative envelope work.
fn fixture() -> TypedMultipartError {
    TypedMultipartError::WrongFieldType {
        field_name: "age".to_owned(),
        wanted: Cow::Borrowed("u8"),
        source: "number too large to fit in target type".to_owned(),
    }
}

/// The offending field name doubles as the envelope `path`.
const PATH: &str = "age";

// ── AFTER: shipped borrowing Serialize chain (mirror of multipart.rs) ─

fn serialize_after(err: &TypedMultipartError) -> Vec<u8> {
    struct Message<'a>(&'a TypedMultipartError);
    impl Serialize for Message<'_> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            // Client-caused variant → stream its `Display` with no `String`.
            s.collect_str(self.0)
        }
    }
    struct OneError<'a> {
        err: &'a TypedMultipartError,
        path: &'a str,
    }
    impl Serialize for OneError<'_> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut st = s.serialize_struct("MultipartOneError", 2)?;
            st.serialize_field("message", &Message(self.err))?;
            st.serialize_field("path", self.path)?;
            st.end()
        }
    }
    struct Envelope<'a> {
        err: &'a TypedMultipartError,
        path: &'a str,
    }
    impl Serialize for Envelope<'_> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut st = s.serialize_struct("MultipartErrorEnvelope", 1)?;
            st.serialize_field(
                "errors",
                &[OneError {
                    err: self.err,
                    path: self.path,
                }],
            )?;
            st.end()
        }
    }
    serde_json::to_vec(&Envelope { err, path: PATH }).expect("infallible")
}

// ── BEFORE: original owned-`String` message implementation ───────────

fn serialize_before(err: &TypedMultipartError) -> Vec<u8> {
    #[derive(Serialize)]
    struct OneError<'a> {
        message: &'a str,
        path: &'a str,
    }
    #[derive(Serialize)]
    struct Envelope<'a> {
        errors: [OneError<'a>; 1],
    }
    let message = err.to_string();
    serde_json::to_vec(&Envelope {
        errors: [OneError {
            message: &message,
            path: PATH,
        }],
    })
    .expect("infallible")
}

fn bench_multipart_error_envelope(c: &mut Criterion) {
    let err = fixture();

    // Guard: the two implementations MUST produce identical bytes, so the
    // A/B compares the same observable work — never a shortcut.
    assert_eq!(
        serialize_before(&err),
        serialize_after(&err),
        "before/after multipart error-envelope bytes diverged"
    );

    let mut group = c.benchmark_group("multipart_error_envelope");
    group.bench_with_input(
        BenchmarkId::new("owned_string_before", "WrongFieldType"),
        &err,
        |b, e| b.iter(|| serialize_before(std::hint::black_box(e))),
    );
    group.bench_with_input(
        BenchmarkId::new("borrowing_serialize_after", "WrongFieldType"),
        &err,
        |b, e| b.iter(|| serialize_after(std::hint::black_box(e))),
    );
    group.finish();
}

criterion_group!(benches, bench_multipart_error_envelope);
criterion_main!(benches);
