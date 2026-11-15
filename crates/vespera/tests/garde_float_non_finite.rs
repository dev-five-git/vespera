//! A `#[schema(minimum / maximum)]` float field rejects a non-finite value.
//!
//! `garde`'s range rule compares `v < min` / `v > max`, and every comparison
//! with `NaN` is false, so `NaN` passed every bound. `serde_urlencoded` and
//! axum's `Path` parse floats with `str::parse`, which accepts `NaN` / `inf`,
//! so `GET /x?score=NaN` reached the handler although the document says
//! `type: number, minimum: 0, maximum: 100` (`NaN` is not a JSON number).

#![cfg(feature = "validation")]

use ::axum::{Router, body::Body, extract::Query, http::Request, routing::get};
use ::tower::ServiceExt;
use ::vespera::__validation::garde::Validate;
use ::vespera::{Schema, Validated};

type Ratio = f64;

#[derive(Schema, serde::Deserialize)]
#[allow(dead_code)]
struct Scores {
    #[schema(minimum = 0, maximum = 100)]
    score: f64,
    #[schema(minimum = 0.5)]
    small: f32,
    #[schema(maximum = 10)]
    maybe: Option<f64>,
    #[schema(value_type = f64, minimum = 0.5, maximum = 2.5)]
    ratio: Ratio,
    #[schema(value_type = f64, minimum = 0)]
    whole: Ratio,
}

fn scores(score: f64, small: f32, maybe: Option<f64>, ratio: f64, whole: f64) -> Scores {
    Scores {
        score,
        small,
        maybe,
        ratio,
        whole,
    }
}

/// The paths `validate` reports, in field order.
fn failing(s: &Scores) -> Vec<String> {
    match s.validate() {
        Ok(()) => Vec::new(),
        Err(report) => report.iter().map(|(path, _)| path.to_string()).collect(),
    }
}

#[test]
fn non_finite_values_fail_every_bounded_float_field() {
    assert_eq!(failing(&scores(50.0, 1.0, Some(1.0), 1.0, 0.0)), [""; 0]);
    assert_eq!(failing(&scores(50.0, 1.0, None, 1.0, 0.0)), [""; 0]);
    assert_eq!(
        failing(&scores(
            f64::NAN,
            f32::NAN,
            Some(f64::NAN),
            f64::NAN,
            f64::NAN
        )),
        ["score", "small", "maybe", "ratio", "whole"]
    );
    assert_eq!(
        failing(&scores(
            f64::INFINITY,
            f32::INFINITY,
            Some(f64::NEG_INFINITY),
            f64::INFINITY,
            f64::INFINITY
        )),
        ["score", "small", "maybe", "ratio", "whole"]
    );
    let report = scores(f64::NAN, 1.0, None, 1.0, 0.0)
        .validate()
        .unwrap_err();
    let messages: Vec<String> = report.iter().map(|(_, e)| e.to_string()).collect();
    assert_eq!(messages, ["not a finite number"]);
}

async fn handler(Validated(Query(_q)): Validated<Query<Scores>>) -> &'static str {
    "ok"
}

async fn status(query: &str) -> u16 {
    let req = Request::builder()
        .uri(format!("/x?{query}"))
        .body(Body::empty())
        .unwrap();
    let res = Router::new()
        .route("/x", get(handler))
        .oneshot(req)
        .await
        .unwrap();
    res.status().as_u16()
}

#[tokio::test]
async fn query_nan_and_inf_are_422() {
    let ok = "score=50&small=1&ratio=1&whole=0";
    assert_eq!(status(ok).await, 200);
    assert_eq!(status(&format!("{ok}&maybe=1")).await, 200);
    assert_eq!(status("score=NaN&small=1&ratio=1&whole=0").await, 422);
    assert_eq!(status("score=50&small=NaN&ratio=1&whole=0").await, 422);
    assert_eq!(status(&format!("{ok}&maybe=NaN")).await, 422);
    assert_eq!(status("score=50&small=1&ratio=NaN&whole=0").await, 422);
    assert_eq!(status("score=50&small=1&ratio=1&whole=inf").await, 422);
}
