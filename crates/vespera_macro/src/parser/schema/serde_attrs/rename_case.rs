//! serde's `rename_all` case conversions, ported verbatim.
//!
//! serde applies a rule DIFFERENTLY to fields and to variants: fields are
//! assumed to be `snake_case` (so `snake_case` / `lowercase` leave them as
//! they are and `camelCase` goes through `PascalCase`), variants are assumed
//! to be `PascalCase` (so `camelCase` only lowercases the first character and
//! `snake_case` splits before every uppercase letter). The two functions below
//! mirror `serde_derive::internals::case::RenameRule::{apply_to_field,
//! apply_to_variant}` exactly, so the documented names are the wire names —
//! `XMLParser` with `camelCase` is `xMLParser`, not `xmlParser`.
//!
//! An unknown rule leaves the name unchanged (serde itself rejects it at
//! compile time).

/// Lowercase the first character (serde's `s[..1].to_ascii_lowercase() + &s[1..]`).
fn lower_first(name: &str) -> String {
    let mut chars = name.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_ascii_lowercase().to_string() + chars.as_str()
    })
}

/// serde's field `PascalCase`: drop `_`, uppercase the character after it.
fn field_pascal(field: &str) -> String {
    let mut pascal = String::with_capacity(field.len());
    let mut capitalize = true;
    for ch in field.chars() {
        if ch == '_' {
            capitalize = true;
        } else if capitalize {
            pascal.push(ch.to_ascii_uppercase());
            capitalize = false;
        } else {
            pascal.push(ch);
        }
    }
    pascal
}

/// serde's variant `snake_case`: `_` before every non-leading uppercase char.
fn variant_snake(variant: &str) -> String {
    let mut snake = String::with_capacity(variant.len() + 4);
    for (i, ch) in variant.char_indices() {
        if i > 0 && ch.is_uppercase() {
            snake.push('_');
        }
        snake.push(ch.to_ascii_lowercase());
    }
    snake
}

/// Rename a struct FIELD (or struct-variant field) per serde's
/// `RenameRule::apply_to_field`.
pub fn rename_field(field_name: &str, rename_all: Option<&str>) -> String {
    match rename_all {
        Some("UPPERCASE" | "SCREAMING_SNAKE_CASE") => field_name.to_ascii_uppercase(),
        Some("PascalCase") => field_pascal(field_name),
        Some("camelCase") => lower_first(&field_pascal(field_name)),
        Some("kebab-case") => field_name.replace('_', "-"),
        Some("SCREAMING-KEBAB-CASE") => field_name.to_ascii_uppercase().replace('_', "-"),
        // `lowercase`, `snake_case`, no rule and unknown rules.
        _ => field_name.to_string(),
    }
}

/// Rename an enum VARIANT per serde's `RenameRule::apply_to_variant`.
pub fn rename_variant(variant_name: &str, rename_all: Option<&str>) -> String {
    match rename_all {
        Some("lowercase") => variant_name.to_ascii_lowercase(),
        Some("UPPERCASE") => variant_name.to_ascii_uppercase(),
        Some("camelCase") => lower_first(variant_name),
        Some("snake_case") => variant_snake(variant_name),
        Some("SCREAMING_SNAKE_CASE") => variant_snake(variant_name).to_ascii_uppercase(),
        Some("kebab-case") => variant_snake(variant_name).replace('_', "-"),
        Some("SCREAMING-KEBAB-CASE") => variant_snake(variant_name)
            .to_ascii_uppercase()
            .replace('_', "-"),
        // `PascalCase`, no rule and unknown rules.
        _ => variant_name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    // Expected values follow serde_derive's `apply_to_field` (fields are
    // taken to be snake_case already).
    #[rstest]
    #[case("user_name", Some("camelCase"), "userName")]
    #[case("user_id_123", Some("camelCase"), "userId123")]
    #[case("UserName", Some("camelCase"), "userName")]
    #[case("ID", Some("camelCase"), "iD")]
    #[case("XMLParser", Some("camelCase"), "xMLParser")]
    #[case("_user", Some("camelCase"), "user")]
    #[case("user_", Some("camelCase"), "user")]
    #[case("", Some("camelCase"), "")]
    #[case("user_name", Some("PascalCase"), "UserName")]
    #[case("user__name", Some("PascalCase"), "UserName")]
    #[case("userName", Some("snake_case"), "userName")]
    #[case("UserName", Some("lowercase"), "UserName")]
    #[case("user_name", Some("UPPERCASE"), "USER_NAME")]
    #[case("firstName", Some("SCREAMING_SNAKE_CASE"), "FIRSTNAME")]
    #[case("user_name", Some("SCREAMING_SNAKE_CASE"), "USER_NAME")]
    #[case("user_name", Some("kebab-case"), "user-name")]
    #[case("UserName", Some("kebab-case"), "UserName")]
    #[case("User_Name", Some("SCREAMING-KEBAB-CASE"), "USER-NAME")]
    #[case("user__name", Some("SCREAMING-KEBAB-CASE"), "USER--NAME")]
    #[case("user_name", None, "user_name")]
    #[case("user_name", Some("unknown"), "user_name")]
    fn field_rules_match_serde(
        #[case] name: &str,
        #[case] rule: Option<&str>,
        #[case] expected: &str,
    ) {
        assert_eq!(rename_field(name, rule), expected);
    }

    // Expected values follow serde_derive's `apply_to_variant` (variants are
    // taken to be PascalCase already).
    #[rstest]
    #[case("UserCreated", Some("camelCase"), "userCreated")]
    #[case("XMLParser", Some("camelCase"), "xMLParser")]
    #[case("HTTPSConnection", Some("camelCase"), "hTTPSConnection")]
    #[case("ID", Some("camelCase"), "iD")]
    #[case("UserName", Some("snake_case"), "user_name")]
    #[case("XMLParser", Some("snake_case"), "x_m_l_parser")]
    #[case("USER", Some("SCREAMING_SNAKE_CASE"), "U_S_E_R")]
    #[case("UserName", Some("SCREAMING_SNAKE_CASE"), "USER_NAME")]
    #[case("UserName", Some("kebab-case"), "user-name")]
    #[case("UserName", Some("SCREAMING-KEBAB-CASE"), "USER-NAME")]
    #[case("UserName", Some("lowercase"), "username")]
    #[case("UserName", Some("UPPERCASE"), "USERNAME")]
    #[case("user_name", Some("PascalCase"), "user_name")]
    #[case("UserName", None, "UserName")]
    #[case("UserName", Some("unknown"), "UserName")]
    fn variant_rules_match_serde(
        #[case] name: &str,
        #[case] rule: Option<&str>,
        #[case] expected: &str,
    ) {
        assert_eq!(rename_variant(name, rule), expected);
    }
}
