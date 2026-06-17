//! VESPERA-04 before/after A/B benchmark for the `422 Unprocessable
//! Entity` validation-envelope serialization.
//!
//! Both implementations serialize the **same** [`garde::Report`] to the
//! **same** bytes (`{"errors":[{"message":...,"path":...},...]}`):
//!
//! - `before`: the original implementation — collect every error into an
//!   owned `Vec<ValidationErrorOut>` (two `String` allocations per error)
//!   and then `serde_json::to_vec`.
//! - `after`: the shipped implementation — a fully-borrowing custom
//!   `Serialize` chain over `&garde::Report` (zero per-error `String`
//!   allocation, `collect_str` straight into the serializer).
//!
//! The delta is the per-error allocation cost VESPERA-04 removes.  Both
//! arms assert byte-identical output so the bench can never silently
//! drift from the real envelope contract.

use std::fmt::Display;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use garde::Validate;
use serde::{Serialize, Serializer, ser::SerializeStruct};

// ── Fixture: a struct whose validation fails on every field ──────────

#[derive(Validate)]
struct Sample {
    #[garde(length(min = 3, max = 32))]
    username: String,
    #[garde(email)]
    email: String,
    #[garde(range(min = 18, max = 120))]
    age: u8,
    #[garde(length(min = 10))]
    bio: String,
    #[garde(url)]
    homepage: String,
}

/// Produce a [`garde::Report`] with `n` failing fields by validating a
/// deliberately-invalid `Sample` and truncating the report's iteration
/// in the benchmarked closures (we just validate the whole struct; it
/// yields 5 errors — representative of a realistic multi-error 422).
fn failing_report() -> garde::Report {
    let sample = Sample {
        username: "x".to_owned(),         // too short
        email: "not-an-email".to_owned(), // invalid
        age: 200,                         // out of range
        bio: "short".to_owned(),          // too short
        homepage: "nope".to_owned(),      // invalid url
    };
    sample.validate().expect_err("sample must fail validation")
}

// ── AFTER: shipped borrowing Serialize chain (mirror of validated.rs) ─

fn serialize_after(report: &garde::Report) -> Vec<u8> {
    struct DisplayValue<T>(T);
    impl<T: Display> Serialize for DisplayValue<T> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.collect_str(&self.0)
        }
    }
    struct Envelope<'a>(&'a garde::Report);
    impl Serialize for Envelope<'_> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut env = s.serialize_struct("ValidationEnvelope", 1)?;
            env.serialize_field("errors", &Errors(self.0))?;
            env.end()
        }
    }
    struct Errors<'a>(&'a garde::Report);
    impl Serialize for Errors<'_> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.collect_seq(self.0.iter().map(|(path, err)| OneError { path, err }))
        }
    }
    struct OneError<'a> {
        path: &'a garde::Path,
        err: &'a garde::Error,
    }
    impl Serialize for OneError<'_> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut e = s.serialize_struct("ValidationError", 2)?;
            e.serialize_field("message", &DisplayValue(self.err.message()))?;
            e.serialize_field("path", &DisplayValue(self.path))?;
            e.end()
        }
    }
    serde_json::to_vec(&Envelope(report)).expect("infallible")
}

// ── BEFORE: original owned-Vec<String> implementation ────────────────

fn serialize_before(report: &garde::Report) -> Vec<u8> {
    #[derive(Serialize)]
    struct ValidationErrorOut {
        message: String,
        path: String,
    }
    #[derive(Serialize)]
    struct Envelope {
        errors: Vec<ValidationErrorOut>,
    }
    let errors: Vec<ValidationErrorOut> = report
        .iter()
        .map(|(path, err)| ValidationErrorOut {
            message: err.message().to_string(),
            path: path.to_string(),
        })
        .collect();
    serde_json::to_vec(&Envelope { errors }).expect("infallible")
}

fn bench_validation_envelope(c: &mut Criterion) {
    let report = failing_report();

    // Guard: the two implementations MUST produce identical bytes, so
    // the A/B compares the same observable work — never a shortcut.
    assert_eq!(
        serialize_before(&report),
        serialize_after(&report),
        "before/after 422 envelope bytes diverged"
    );

    let n_errors = report.iter().count();
    let mut group = c.benchmark_group("validation_envelope");

    group.bench_with_input(
        BenchmarkId::new("owned_vec_string_before", n_errors),
        &report,
        |b, report| b.iter(|| serialize_before(std::hint::black_box(report))),
    );
    group.bench_with_input(
        BenchmarkId::new("borrowing_serialize_after", n_errors),
        &report,
        |b, report| b.iter(|| serialize_after(std::hint::black_box(report))),
    );

    group.finish();
}

criterion_group!(benches, bench_validation_envelope);
criterion_main!(benches);
