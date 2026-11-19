//! `inline_module_of` (what `#[route]` stores as a handler's inline module)
//! locates a handler in its source. Split out of `route_impl.rs` to keep that
//! file under the 1000-line cap.

/// `inline_module_of` — what `#[route]` stores as the handler's
/// inline-module path — finds a route handler's enclosing inline module
/// (and the file root) in its source.  `#[route]` itself has no
/// call-site file under unit tests (`inline_module` is `None` there),
/// so this exercises the lookup, not the attribute.
#[test]
fn test_inline_module_of_locates_route_handlers() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("scoped_users.rs");
    std::fs::write(
        &file,
        "pub mod v2 {\n    #[route(get)]\n    pub async fn scoped_list() {}\n}\n",
    )
    .unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    let path = file.to_str();
    assert_eq!(
        crate::cron_impl::inline_module_of(
            path,
            "scoped_list",
            proc_macro2::LineColumn {
                line: 3,
                column: 17
            },
        ),
        Some("v2".to_string())
    );
    std::fs::write(&file, "pub async fn scoped_list() {}\n").unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    assert_eq!(
        crate::cron_impl::inline_module_of(
            path,
            "scoped_list",
            proc_macro2::LineColumn {
                line: 1,
                column: 13
            },
        ),
        Some(String::new())
    );
}
