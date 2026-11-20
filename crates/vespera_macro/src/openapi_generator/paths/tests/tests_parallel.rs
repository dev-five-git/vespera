//! Parallel worker plumbing of the path jobs.

use crate::openapi_generator::paths::{PARALLEL_THRESHOLD, parallel_filter_map_on};

/// R32-MR3-01: a parallel worker reads the call-site module its caller read
/// (a worker's own span has no file), so the parallel and the sequential path
/// resolve an ecosystem-named type at the same module.
#[test]
fn parallel_workers_see_the_callers_call_site_module() {
    let _call_site = crate::schema_macro::set_test_call_site_module("crate::routes::money");
    let jobs: Vec<usize> = (0..(PARALLEL_THRESHOLD * 2)).collect();
    let probe = |_: &usize| -> syn::Result<Option<Option<Vec<String>>>> {
        Ok(Some(crate::schema_macro::call_site_module()))
    };
    let expected = vec![
        Some(vec![
            "crate".to_string(),
            "routes".to_string(),
            "money".to_string(),
        ]);
        jobs.len()
    ];
    let sequential = parallel_filter_map_on(&jobs, &probe, 1).expect("sequential");
    let parallel = parallel_filter_map_on(&jobs, &probe, 2).expect("parallel");
    assert_eq!(sequential, expected);
    assert_eq!(parallel, expected);
}
