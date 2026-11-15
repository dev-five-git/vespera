use quote::quote;

use super::*;

#[test]
fn feature_disabled_stand_in_keeps_the_item_and_names_the_feature() {
    let item = quote!(
        pub async fn job() {}
    );
    let out = feature_disabled_error(item.clone()).to_string();
    assert!(out.starts_with(":: core :: compile_error !"), "{out}");
    assert!(
        out.contains(r#"vespera = { features = [\"cron\"] }"#),
        "{out}"
    );
    assert!(out.ends_with(&item.to_string()), "{out}");
    // Not a function: still an error (spanned on the item), item kept.
    let out = feature_disabled_error(quote!(
        struct S;
    ))
    .to_string();
    assert!(out.contains("compile_error"), "{out}");
    assert!(out.ends_with("struct S ;"), "{out}");
}

// ========== Tests for validate_cron_fn ==========

#[test]
fn test_validate_cron_fn_not_public() {
    let item: syn::ItemFn = syn::parse_quote! {
        async fn private_job() {
            println!("job");
        }
    };
    let result = validate_cron_fn(&item);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("function must be public")
    );
}

#[test]
fn test_validate_cron_fn_not_async() {
    let item: syn::ItemFn = syn::parse_quote! {
        pub fn sync_job() {
            println!("job");
        }
    };
    let result = validate_cron_fn(&item);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("function must be async")
    );
}

/// The scheduler awaits a `()` future: any other return type is rejected at
/// the job (it failed with an `Output` mismatch inside `vespera!`).
#[rstest::rstest]
#[case::result(syn::parse_quote!(pub async fn sync() -> Result<(), String> {}), Some("must return `()`"))]
#[case::value(syn::parse_quote!(pub async fn count() -> usize { 0 }), Some("must return `()`"))]
#[case::explicit_unit(syn::parse_quote!(pub async fn unit() -> () {}), None)]
#[case::default_unit(syn::parse_quote!(pub async fn plain() {}), None)]
fn test_validate_cron_fn_return_type(#[case] item: syn::ItemFn, #[case] message: Option<&str>) {
    let error = validate_cron_fn(&item).err().map(|e| e.to_string());
    match message {
        Some(message) => assert!(
            error.as_deref().is_some_and(|e| e.contains(message)),
            "{error:?}"
        ),
        None => assert_eq!(error, None),
    }
}

#[test]
fn test_validate_cron_fn_has_params() {
    let item: syn::ItemFn = syn::parse_quote! {
        pub async fn job_with_params(x: i32) {
            println!("{}", x);
        }
    };
    let result = validate_cron_fn(&item);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("must take no parameters")
    );
}

#[rstest::rstest]
#[case::generic(syn::parse_quote!(pub async fn job<T>() {}), "must not be generic")]
#[case::where_clause(syn::parse_quote!(pub async fn job() where u8: Copy {}), "must not be generic")]
#[case::unsafe_fn(syn::parse_quote!(pub async unsafe fn job() {}), "must not be `unsafe`")]
fn test_validate_cron_fn_rejects_uncallable_signatures(
    #[case] item: syn::ItemFn,
    #[case] message: &str,
) {
    let error = validate_cron_fn(&item).unwrap_err().to_string();
    assert!(error.contains(message), "{error}");
}

/// A cron function in an `impl` / `trait` block or a function body has
/// no `crate::module::function` path; a free one (top level or in an
/// inline module) is fine, and an unreadable file is not checked.
/// Statements and items that declare no function are passed over; a
/// `const` / `static` initialiser, a closure, an inner block and an
/// `async` block are function-local scopes too.
#[rstest::rstest]
#[case::top_level(2, true)]
#[case::inline_module(3, true)]
#[case::impl_method(5, false)]
#[case::trait_default(8, false)]
#[case::nested_in_fn(11, false)]
#[case::nested_in_method(15, false)]
#[case::nested_in_trait_default(20, false)]
#[case::const_initialiser(25, false)]
#[case::static_initialiser(26, false)]
#[case::closure_body(27, false)]
#[case::inner_block(28, false)]
#[case::async_block(29, false)]
#[case::module_after_items(30, true)]
fn test_check_free_function(#[case] line: usize, #[case] free: bool) {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("jobs.rs");
    std::fs::write(
        &file,
        "\npub async fn tick() {}\nmod a { pub async fn tick() {} }\nimpl S {\n    pub async fn tick() {}\n}\ntrait T {\n    async fn tick() {}\n}\npub fn outer() {\n    let _x = 1; pub async fn tick() {}\n}\nimpl S {\n    pub fn method() {\n        pub async fn tick() {}\n    }\n}\ntrait U {\n    fn provided() {\n        pub async fn tick() {}\n    }\n    fn required();\n}\npub struct S;\nconst _: () = { pub async fn tick() {} };\nstatic X: () = { pub async fn tick() {} };\npub fn closure() { let _c = || { pub async fn tick() {} }; }\npub fn blocky() { { pub async fn tick() {} } }\npub fn asyncb() { let _f = async { pub async fn tick() {} }; }\nmod late { pub async fn tick() {} }\n",
    )
    .unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    let item: syn::ItemFn = syn::parse_quote!(
        pub async fn tick() {}
    );
    let file = file.display().to_string();
    assert_eq!(check_free_function(Some(&file), &item, line).is_ok(), free);
    assert!(check_free_function(None, &item, line).is_ok());
}

#[test]
fn test_validate_cron_fn_valid() {
    let item: syn::ItemFn = syn::parse_quote! {
        pub async fn valid_job() {
            println!("job");
        }
    };
    let result = validate_cron_fn(&item);
    assert!(result.is_ok());
}

// ========== Tests for process_cron_attribute ==========

#[test]
fn test_process_cron_attribute_valid() {
    let attr = quote!("0 */5 * * * *");
    let item = quote!(
        pub async fn my_job() {
            println!("running");
        }
    );
    let result = process_cron_attribute(attr, item.clone());
    let output = result.unwrap().to_string();
    // The item is kept, followed by the spanned `Send` check of its future.
    assert!(output.starts_with(&item.to_string()), "{output}");
    assert!(
        output.ends_with(
            "const _ : () = { fn __vespera_cron_job_must_be_send < T : :: core :: marker :: Send > (_ : T) { } # [allow (dead_code)] fn __vespera_cron_send_check () { __vespera_cron_job_must_be_send (my_job ()) ; } } ;"
        ),
        "{output}"
    );
}

/// A rejected `#[cron]` keeps the function next to its error: the module
/// scan may still schedule it, and a dropped function would add an
/// unresolved-name error inside the `vespera!` expansion.
#[test]
fn rejected_cron_keeps_the_item() {
    let item = quote!(
        pub async fn job() {}
    );
    let output = cron_attribute(quote!("0 * * * * *", overlap = "queue"), item).to_string();
    assert!(output.starts_with(":: core :: compile_error !"), "{output}");
    assert!(output.ends_with("pub async fn job () { }"), "{output}");
}

#[test]
fn accepted_cron_emits_the_expansion() {
    let output = cron_attribute(
        quote!("0 * * * * *"),
        quote!(
            pub async fn accepted_job() {}
        ),
    )
    .to_string();
    assert!(
        output.starts_with("pub async fn accepted_job () { }"),
        "{output}"
    );
    assert!(output.contains("__vespera_cron_send_check"), "{output}");
}

#[test]
fn test_process_cron_attribute_invalid_expression() {
    let attr = quote!(123);
    let item = quote!(
        pub async fn my_job() {
            println!("running");
        }
    );
    let result = process_cron_attribute(attr, item);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("expected a cron expression string"));
}

#[test]
fn test_process_cron_attribute_not_function() {
    let attr = quote!("0 * * * * *");
    let item = quote!(
        struct NotAFunction;
    );
    let result = process_cron_attribute(attr, item);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("can only be applied to functions"));
}

#[test]
fn test_process_cron_attribute_not_public() {
    let attr = quote!("0 * * * * *");
    let item = quote!(
        async fn private_job() {
            println!("job");
        }
    );
    let result = process_cron_attribute(attr, item);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("function must be public"));
}

#[test]
fn test_process_cron_attribute_not_async() {
    let attr = quote!("0 * * * * *");
    let item = quote!(
        pub fn sync_job() {
            println!("job");
        }
    );
    let result = process_cron_attribute(attr, item);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("function must be async"));
}

#[test]
fn test_process_cron_attribute_with_params() {
    let attr = quote!("0 * * * * *");
    let item = quote!(
        pub async fn job(x: i32) {
            println!("{}", x);
        }
    );
    let result = process_cron_attribute(attr, item);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("must take no parameters"));
}

#[test]
fn test_register_cron_replaces_same_file_and_function() {
    let file_path = Some("/tmp/vespera/tasks/replaced.rs".to_string());
    let fn_name = "__test_replace_cron".to_string();
    register_cron(StoredCronInfo {
        fn_name: fn_name.clone(),
        expression: "0 */5 * * * *".to_string(),
        file_path: file_path.clone(),
        inline_module: None,
        options: CronOptions::default(),
    });
    register_cron(StoredCronInfo {
        fn_name: fn_name.clone(),
        expression: "0 */10 * * * *".to_string(),
        file_path,
        inline_module: None,
        options: CronOptions::default(),
    });

    let matches: Vec<_> = current_crate_crons()
        .iter()
        .filter(|entry| entry.fn_name == fn_name)
        .cloned()
        .collect();
    assert_eq!(matches.len(), 1, "same source cron should replace");
    assert_eq!(matches[0].expression, "0 */10 * * * *");
}

/// Same file + function name but different inline modules are two jobs;
/// an unknown inline module still replaces (IDE re-expansion).
#[test]
fn test_register_cron_keys_by_inline_module() {
    let file_path = Some("/tmp/vespera/tasks/clash.rs".to_string());
    let fn_name = "__test_clash_cron".to_string();
    let entry = |inline: Option<&str>, expression: &str| StoredCronInfo {
        fn_name: fn_name.clone(),
        expression: expression.to_string(),
        file_path: file_path.clone(),
        inline_module: inline.map(str::to_string),
        options: CronOptions::default(),
    };
    let expressions = || -> Vec<(Option<String>, String)> {
        current_crate_crons()
            .iter()
            .filter(|entry| entry.fn_name == fn_name)
            .map(|entry| (entry.inline_module.clone(), entry.expression.clone()))
            .collect()
    };
    register_cron(entry(Some("a"), "1 * * * * *"));
    register_cron(entry(Some("b"), "2 * * * * *"));
    register_cron(entry(Some("a"), "3 * * * * *"));
    assert_eq!(
        expressions(),
        vec![
            (Some("a".to_string()), "3 * * * * *".to_string()),
            (Some("b".to_string()), "2 * * * * *".to_string()),
        ]
    );
    register_cron(entry(None, "4 * * * * *"));
    assert_eq!(expressions().len(), 2, "unknown scope replaces, never adds");
}

#[rstest::rstest]
#[case::file_level("tick", 2, Some(""))]
#[case::inline_a("tick", 5, Some("a"))]
#[case::nested_raw_b("tick", 9, Some("b::inner"))]
#[case::wrong_line("tick", 3, None)]
#[case::unknown_fn("tock", 5, None)]
#[case::raw_name("r#match", 13, Some("c"))]
#[case::unraw_name("match", 13, Some("c"))]
fn test_inline_module_of_locates_the_declaration(
    #[case] fn_name: &str,
    #[case] line: usize,
    #[case] expected: Option<&str>,
) {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("jobs.rs");
    std::fs::write(
        &file,
        "\n#[cron(\"0 * * * * *\")] pub async fn tick() {}\nmod a {\n    pub fn other() {}\n    #[cron(\"1 * * * * *\")] pub async fn tick() {}\n}\nmod r#b {\n    mod inner {\n        #[cron(\"2 * * * * *\")] pub async fn tick() {}\n    }\n    mod external;\n}\nmod c { pub async fn r#match() {} }\n",
    )
    .unwrap();
    let file = file.display().to_string();
    assert_eq!(
        inline_module_of(Some(&file), fn_name, line),
        expected.map(str::to_string)
    );
}

#[rstest::rstest]
#[case::no_file(None)]
#[case::missing_file(Some("/definitely/not/here/jobs.rs"))]
fn test_inline_module_of_without_a_readable_file(#[case] file: Option<&str>) {
    assert_eq!(inline_module_of(file, "tick", 1), None);
}

#[test]
fn test_inline_module_of_unparsable_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("broken.rs");
    std::fs::write(&file, "mod {{{").unwrap();
    assert_eq!(
        inline_module_of(Some(&file.display().to_string()), "tick", 1),
        None
    );
}

/// Every handler of one file shares ONE parse: `#[route]` asks for each
/// handler's inline module, `#[cron]` additionally checks it is free — the
/// file's function index is built once per content version, and an edit
/// rebuilds it.
#[test]
fn test_fn_index_parses_each_file_version_once() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("routes.rs");
    std::fs::write(
        &path,
        "mod a { pub async fn x() {} }\npub async fn y() {}\n",
    )
    .unwrap();
    let file = path.display().to_string();
    crate::schema_macro::file_cache::bump_epoch();
    let before = crate::schema_macro::file_cache::derived_computes();
    let item: syn::ItemFn = syn::parse_quote!(
        pub async fn y() {}
    );
    for _ in 0..3 {
        crate::schema_macro::file_cache::bump_epoch();
        assert_eq!(inline_module_of(Some(&file), "x", 1).as_deref(), Some("a"));
        assert_eq!(inline_module_of(Some(&file), "y", 2).as_deref(), Some(""));
        assert!(check_free_function(Some(&file), &item, 2).is_ok());
    }
    assert_eq!(
        crate::schema_macro::file_cache::derived_computes() - before,
        1
    );

    std::fs::write(
        &path,
        "pub async fn y() {}\nmod bb { pub async fn x() {} }\n",
    )
    .unwrap();
    crate::schema_macro::file_cache::bump_epoch();
    assert_eq!(inline_module_of(Some(&file), "x", 2).as_deref(), Some("bb"));
    assert_eq!(
        crate::schema_macro::file_cache::derived_computes() - before,
        2
    );
}

// ===== Compile-time cron-syntax validation (gated by the `cron` feature) =====

#[cfg(feature = "cron")]
#[test]
fn test_process_cron_attribute_valid_cron_syntax_passes() {
    for expr in [
        quote!("0 */5 * * * *"),
        quote!("1/10 * * * * *"),
        quote!("0 0 0 * * *"),
        quote!("0 30 9 * * Mon-Fri"),
    ] {
        let item = quote!(
            pub async fn my_job() {}
        );
        assert!(
            process_cron_attribute(expr.clone(), item).is_ok(),
            "expected valid cron `{expr}` to pass"
        );
    }
}

#[cfg(feature = "cron")]
#[test]
fn test_process_cron_attribute_invalid_cron_syntax_is_compile_error() {
    // Each is rejected at compile time (was a runtime scheduler failure):
    // 1-field, 5-field (missing seconds), out-of-range minute, garbage token.
    for bad in [
        quote!("invalid"),
        quote!("* * * * *"),
        quote!("0 99 * * * *"),
        quote!("not a cron at all"),
    ] {
        let item = quote!(
            pub async fn my_job() {}
        );
        let result = process_cron_attribute(bad.clone(), item);
        assert!(result.is_err(), "expected invalid cron `{bad}` to error");
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("invalid cron expression"),
            "expected `invalid cron expression` message for `{bad}`"
        );
    }
}

// ===== `tz` / `overlap` arguments =====

/// The optional arguments parse in any order, with a trailing comma.
#[rstest::rstest]
#[case::expression_only(quote!("0 * * * * *"), CronOptions::default())]
#[case::trailing_comma(quote!("0 * * * * *",), CronOptions::default())]
#[case::overlap_allow(quote!("0 * * * * *", overlap = "allow"), CronOptions::default())]
#[case::overlap_skip(
    quote!("0 * * * * *", overlap = "skip"),
    CronOptions { timezone: None, skip_overlap: true }
)]
#[case::tz_and_overlap(
    quote!("0 * * * * *", overlap = "skip", tz = "Asia/Seoul",),
    CronOptions { timezone: Some("Asia/Seoul".to_owned()), skip_overlap: true }
)]
fn cron_args_parse_optional_arguments(
    #[case] attr: proc_macro2::TokenStream,
    #[case] expected: CronOptions,
) {
    let args: CronArgs = syn::parse2(attr).expect("valid arguments");
    assert_eq!(args.expression.value(), "0 * * * * *");
    assert_eq!(args.options(), expected);
}

/// Malformed optional arguments are span-attached compile errors.
#[rstest::rstest]
#[case::unknown_key(quote!("0 * * * * *", zone = "UTC"), "unknown argument `zone`")]
#[case::duplicate_tz(quote!("0 * * * * *", tz = "UTC", tz = "UTC"), "duplicate `tz` argument")]
#[case::duplicate_overlap(
    quote!("0 * * * * *", overlap = "skip", overlap = "skip"),
    "duplicate `overlap` argument"
)]
#[case::unknown_overlap(quote!("0 * * * * *", overlap = "queue"), "unknown overlap policy `queue`")]
#[case::missing_comma(quote!("0 * * * * *" tz = "UTC"), "expected `,`")]
#[case::missing_equals(quote!("0 * * * * *", tz "UTC"), "expected `=`")]
#[case::non_literal_value(quote!("0 * * * * *", tz = UTC), "expected string literal")]
fn cron_args_reject_malformed_arguments(
    #[case] attr: proc_macro2::TokenStream,
    #[case] message: &str,
) {
    let error = syn::parse2::<CronArgs>(attr)
        .err()
        .expect("malformed arguments must fail")
        .to_string();
    assert!(error.contains(message), "{error}");
}

/// `process_cron_attribute` stores the parsed options for `vespera!`.
#[test]
fn process_cron_attribute_stores_overlap_policy() {
    let item = quote!(
        pub async fn __test_overlap_skip_job() {}
    );
    process_cron_attribute(quote!("0 * * * * *", overlap = "skip"), item).expect("valid");
    let stored = current_crate_crons()
        .iter()
        .find(|entry| entry.fn_name == "__test_overlap_skip_job")
        .cloned()
        .expect("registered");
    assert!(stored.options.skip_overlap);
    assert_eq!(stored.options.timezone, None);
}

/// A raw-identifier job is stored unraw (`match`), the name the
/// module-tree scan and `inline_module_of` use: otherwise the stored and
/// the scanned copy of one job never match and it is scheduled twice.
#[test]
fn process_cron_attribute_stores_raw_identifiers_unraw() {
    let item = quote!(
        pub async fn r#__test_raw_job() {}
    );
    process_cron_attribute(quote!("0 * * * * *"), item).expect("valid");
    assert!(
        current_crate_crons()
            .iter()
            .any(|entry| entry.fn_name == "__test_raw_job")
    );
    let dir = tempfile::TempDir::new().unwrap();
    let file = dir.path().join("jobs.rs");
    std::fs::write(
        &file,
        "mod a {
pub async fn r#match() {}
}
",
    )
    .unwrap();
    assert_eq!(
        inline_module_of(Some(&file.display().to_string()), "match", 2),
        Some("a".to_owned())
    );
}

#[cfg(feature = "cron-tz")]
#[rstest::rstest]
#[case::known("Asia/Seoul", None)]
#[case::utc("UTC", None)]
#[case::wrong_case("asia/seoul", Some("unknown time zone `asia/seoul`"))]
#[case::unknown("Mars/Olympus_Mons", Some("unknown time zone `Mars/Olympus_Mons`"))]
fn process_cron_attribute_validates_time_zones(
    #[case] timezone: &str,
    #[case] error: Option<&str>,
) {
    let item = quote!(
        pub async fn my_job() {}
    );
    let result = process_cron_attribute(quote!("0 0 9 * * *", tz = #timezone), item);
    match error {
        None => assert!(result.is_ok(), "{timezone}"),
        Some(message) => {
            let error = result.expect_err("unknown zone must fail").to_string();
            assert!(error.contains(message), "{error}");
        }
    }
}

#[cfg(not(feature = "cron-tz"))]
#[test]
fn tz_without_cron_tz_feature_is_a_compile_error() {
    let item = quote!(
        pub async fn my_job() {}
    );
    let error = process_cron_attribute(quote!("0 0 9 * * *", tz = "Asia/Seoul"), item)
        .expect_err("tz needs the cron-tz feature")
        .to_string();
    assert!(error.contains("requires the `cron-tz` feature"), "{error}");
}
