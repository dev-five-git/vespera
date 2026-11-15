//! Request `Content-Type` defaulting knob: whether a non-empty request body
//! that arrived **without** a `Content-Type` is given
//! `content-type: application/json` before it reaches the router.  Same
//! resolution policy as every other knob in `config` (setter > env var >
//! default, then process-fixed via `OnceLock`; an invalid env value warns
//! once and falls back to the default).

use std::sync::OnceLock;

/// Default of [`default_json_content_type`]: `true` (the historical
/// behaviour).
pub const DEFAULT_DEFAULT_JSON_CONTENT_TYPE: bool = true;

static DEFAULT_JSON_CONTENT_TYPE: OnceLock<bool> = OnceLock::new();

const TRUE_WORDS: [&str; 4] = ["true", "1", "yes", "on"];
const FALSE_WORDS: [&str; 4] = ["false", "0", "no", "off"];

/// Parse an optional boolean config string: `true` / `1` / `yes` / `on` and
/// `false` / `0` / `no` / `off` (ASCII case-insensitive, surrounding
/// whitespace ignored).  Absent → `default`; present but unrecognised →
/// one-time stderr warning (the caller resolves through a `OnceLock`) and
/// `default`, the same warn-and-default policy as the numeric knobs.
fn parse_bool_config(var_name: &str, raw: Option<&str>, default: bool) -> bool {
    let Some(s) = raw else {
        return default;
    };
    let value = s.trim();
    let is = |words: &[&str]| words.iter().any(|w| value.eq_ignore_ascii_case(w));
    if is(&TRUE_WORDS) {
        true
    } else if is(&FALSE_WORDS) {
        false
    } else {
        eprintln!(
            "vespera: ignoring invalid {var_name}={s:?} (expected true or false); using the default {default}"
        );
        default
    }
}

/// Whether a non-empty request body without a `Content-Type` is given
/// `content-type: application/json` (an empty body never is; bidirectional
/// streaming treats a request declaring `Content-Length: 0` as empty).
///
/// Resolution order (first hit wins, then cached for the process lifetime):
/// [`set_default_json_content_type`] > `VESPERA_DEFAULT_JSON_CONTENT_TYPE`
/// env var (`true` / `false`) > [`DEFAULT_DEFAULT_JSON_CONTENT_TYPE`]
/// (`true`).  Every wire dispatch (buffered, direct, response-streaming,
/// bidirectional) and the text-envelope API read it, so the first dispatch
/// fixes it.
///
/// **Security (CSRF):** with the default, axum's `Json<T>` accepts a body
/// that arrived with no `Content-Type` — which a browser can send
/// cross-site as a CORS "simple" request without a preflight — so its
/// content-type check is no CSRF defence.  Browser-facing apps should set
/// this to `false`: such a body then reaches the handler without a
/// `Content-Type`, and `Json<T>` rejects it with `415`.
#[must_use]
#[inline]
pub fn default_json_content_type() -> bool {
    *DEFAULT_JSON_CONTENT_TYPE.get_or_init(|| {
        let var_name = "VESPERA_DEFAULT_JSON_CONTENT_TYPE";
        let raw = std::env::var(var_name).ok();
        parse_bool_config(var_name, raw.as_deref(), DEFAULT_DEFAULT_JSON_CONTENT_TYPE)
    })
}

/// Override [`default_json_content_type`] **before the first dispatch**.
/// Returns `false` when the value was already fixed (a previous call or a
/// dispatch already read it).
pub fn set_default_json_content_type(enabled: bool) -> bool {
    DEFAULT_JSON_CONTENT_TYPE.set(enabled).is_ok()
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_DEFAULT_JSON_CONTENT_TYPE, default_json_content_type, parse_bool_config,
        set_default_json_content_type,
    };
    use rstest::rstest;

    /// Process-global `OnceLock`: run in a child process so the setter
    /// cannot race other unit tests that dispatch with the default.
    #[test]
    #[cfg_attr(miri, ignore = "re-executes the test binary as a child process")]
    fn setter_is_first_wins_and_wins_over_the_env_var() {
        const CHILD_ENV: &str = "VESPERA_CONTENT_TYPE_SETTER_TEST_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            let output = std::process::Command::new(
                std::env::current_exe().expect("current unit-test executable"),
            )
            .args([
                "--exact",
                "config::content_type::tests::setter_is_first_wins_and_wins_over_the_env_var",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .env("VESPERA_DEFAULT_JSON_CONTENT_TYPE", "true")
            .output()
            .expect("run isolated content-type child test");
            assert!(
                output.status.success(),
                "isolated content-type test failed:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        assert!(set_default_json_content_type(false));
        assert!(!default_json_content_type(), "the setter beats the env var");
        assert!(!set_default_json_content_type(true), "first-wins");
        assert!(!default_json_content_type());
    }

    /// The env var alone (no setter) is honoured.
    #[test]
    #[cfg_attr(miri, ignore = "re-executes the test binary as a child process")]
    fn env_var_is_read_when_no_setter_ran() {
        const CHILD_ENV: &str = "VESPERA_CONTENT_TYPE_ENV_TEST_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            let output = std::process::Command::new(
                std::env::current_exe().expect("current unit-test executable"),
            )
            .args([
                "--exact",
                "config::content_type::tests::env_var_is_read_when_no_setter_ran",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .env("VESPERA_DEFAULT_JSON_CONTENT_TYPE", " Off ")
            .output()
            .expect("run isolated content-type env child test");
            assert!(
                output.status.success(),
                "isolated content-type env test failed:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        assert!(!default_json_content_type());
        assert!(!set_default_json_content_type(true), "fixed by the read");
    }

    #[rstest]
    #[case::absent(None, DEFAULT_DEFAULT_JSON_CONTENT_TYPE)]
    #[case::false_word(Some("false"), false)]
    #[case::false_upper_padded(Some("  FALSE "), false)]
    #[case::zero(Some("0"), false)]
    #[case::no(Some("No"), false)]
    #[case::off(Some("off"), false)]
    #[case::true_word(Some("true"), true)]
    #[case::one(Some("1"), true)]
    #[case::yes(Some("YES"), true)]
    #[case::on(Some(" on"), true)]
    #[case::invalid(Some("nope"), DEFAULT_DEFAULT_JSON_CONTENT_TYPE)]
    #[case::empty(Some(""), DEFAULT_DEFAULT_JSON_CONTENT_TYPE)]
    fn bool_policy(#[case] raw: Option<&str>, #[case] expected: bool) {
        assert_eq!(
            parse_bool_config(
                "VESPERA_DEFAULT_JSON_CONTENT_TYPE",
                raw,
                DEFAULT_DEFAULT_JSON_CONTENT_TYPE
            ),
            expected
        );
    }

    #[test]
    fn invalid_value_falls_back_to_a_false_default_too() {
        assert!(!parse_bool_config("X", Some("maybe"), false));
    }
}
