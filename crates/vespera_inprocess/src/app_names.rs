//! Compile-time app-name checks shared by the host registration macros
//! (`vespera_jni::jni_apps!`, [`ffi_apps!`](crate::ffi_apps)): the same rules
//! [`crate::register_app_named`] applies at runtime, evaluated in a `const`
//! so an invalid or duplicated name is a build error.

/// Every name is non-empty, ≤ 64 bytes and only `[A-Za-z0-9_-]` after
/// trimming.
#[doc(hidden)]
#[must_use]
pub const fn __app_names_valid(names: &[&str]) -> bool {
    let mut i = 0;
    while i < names.len() {
        let name = names[i].as_bytes().trim_ascii();
        if name.is_empty() || name.len() > 64 {
            return false;
        }
        let mut j = 0;
        while j < name.len() {
            let b = name[j];
            if !(b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

/// No two names are equal after trimming (the registry trims before keying).
#[doc(hidden)]
#[must_use]
pub const fn __app_names_unique(names: &[&str]) -> bool {
    let mut i = 0;
    while i < names.len() {
        let mut j = i + 1;
        while j < names.len() {
            if const_bytes_eq(
                names[i].as_bytes().trim_ascii(),
                names[j].as_bytes().trim_ascii(),
            ) {
                return false;
            }
            j += 1;
        }
        i += 1;
    }
    true
}

const fn const_bytes_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    #[rstest]
    #[case::single_default(&["_default"], true, true)]
    #[case::several(&["_default", "admin", "public-v2"], true, true)]
    #[case::duplicate(&["admin", "admin"], true, false)]
    #[case::duplicate_after_trim(&["admin", " admin "], true, false)]
    #[case::same_len_differs(&["abc", "abd"], true, true)]
    #[case::empty(&["  "], false, true)]
    #[case::bad_char(&["ad min"], false, true)]
    #[case::too_long(&["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"], false, true)]
    fn compile_time_app_name_checks(
        #[case] names: &[&str],
        #[case] valid: bool,
        #[case] unique: bool,
    ) {
        assert_eq!(super::__app_names_valid(names), valid);
        assert_eq!(super::__app_names_unique(names), unique);
    }
}
