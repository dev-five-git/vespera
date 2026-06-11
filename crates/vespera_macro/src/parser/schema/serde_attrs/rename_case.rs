#[allow(clippy::too_many_lines)]
pub fn rename_field(field_name: &str, rename_all: Option<&str>) -> String {
    // "lowercase", "UPPERCASE", "PascalCase", "camelCase", "snake_case", "SCREAMING_SNAKE_CASE", "kebab-case", "SCREAMING-KEBAB-CASE"
    match rename_all {
        Some("camelCase") => {
            // Convert snake_case or PascalCase to camelCase
            let mut result = String::new();
            let mut capitalize_next = false;
            let mut in_first_word = true;
            let chars: Vec<char> = field_name.chars().collect();

            for (i, &ch) in chars.iter().enumerate() {
                if ch == '_' {
                    capitalize_next = true;
                    in_first_word = false;
                    continue;
                }
                if in_first_word {
                    // In first word: lowercase until we hit a word boundary
                    // Word boundary: uppercase char followed by lowercase (e.g., "XMLParser" -> "P" starts new word)
                    let next_is_lower = chars.get(i + 1).is_some_and(|c| c.is_lowercase());
                    if ch.is_uppercase() && next_is_lower && i > 0 {
                        // This uppercase starts a new word (e.g., 'P' in "XMLParser")
                        in_first_word = false;
                        result.push(ch);
                    } else {
                        // Still in first word, lowercase it
                        result.push(ch.to_ascii_lowercase());
                    }
                    continue;
                }
                if capitalize_next {
                    result.push(ch.to_ascii_uppercase());
                    capitalize_next = false;
                    continue;
                }
                result.push(ch);
            }
            result
        }
        Some("snake_case") => {
            // Convert camelCase to snake_case
            let mut result = String::new();
            for (i, ch) in field_name.chars().enumerate() {
                if ch.is_uppercase() && i > 0 {
                    result.push('_');
                }
                result.push(ch.to_ascii_lowercase());
            }
            result
        }
        Some("kebab-case") => {
            // Convert snake_case or Camel/PascalCase to kebab-case (lowercase with hyphens)
            let mut result = String::new();
            for (i, ch) in field_name.chars().enumerate() {
                if ch.is_uppercase() {
                    if i > 0 && !result.ends_with('-') {
                        result.push('-');
                    }
                    result.push(ch.to_ascii_lowercase());
                } else if ch == '_' {
                    result.push('-');
                } else {
                    result.push(ch);
                }
            }
            result
        }
        Some("PascalCase") => {
            // Convert snake_case to PascalCase
            let mut result = String::new();
            let mut capitalize_next = true;
            for ch in field_name.chars() {
                if ch == '_' {
                    capitalize_next = true;
                } else if capitalize_next {
                    result.push(ch.to_ascii_uppercase());
                    capitalize_next = false;
                } else {
                    result.push(ch);
                }
            }
            result
        }
        Some("lowercase") => {
            // Convert to lowercase
            field_name.to_lowercase()
        }
        Some("UPPERCASE") => {
            // Convert to UPPERCASE
            field_name.to_uppercase()
        }
        Some("SCREAMING_SNAKE_CASE") => {
            // Convert to SCREAMING_SNAKE_CASE
            // If already in SCREAMING_SNAKE_CASE format, return as is
            if field_name.chars().all(|c| c.is_uppercase() || c == '_') && field_name.contains('_')
            {
                return field_name.to_string();
            }
            // First convert to snake_case if needed, then uppercase
            let mut snake_case = String::new();
            for (i, ch) in field_name.chars().enumerate() {
                if ch.is_uppercase() && i > 0 && !snake_case.ends_with('_') {
                    snake_case.push('_');
                }
                if ch != '_' && ch != '-' {
                    snake_case.push(ch.to_ascii_lowercase());
                } else if ch == '_' {
                    snake_case.push('_');
                }
            }
            snake_case.to_uppercase()
        }
        Some("SCREAMING-KEBAB-CASE") => {
            // Convert to SCREAMING-KEBAB-CASE
            // First convert to kebab-case if needed, then uppercase
            let mut kebab_case = String::new();
            for (i, ch) in field_name.chars().enumerate() {
                if ch.is_uppercase()
                    && i > 0
                    && !kebab_case.ends_with('-')
                    && !kebab_case.ends_with('_')
                {
                    kebab_case.push('-');
                }
                if ch == '_' {
                    kebab_case.push('-');
                } else if ch != '-' {
                    kebab_case.push(ch.to_ascii_lowercase());
                } else {
                    kebab_case.push('-');
                }
            }
            kebab_case.to_uppercase()
        }
        _ => field_name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    #[rstest]
    // camelCase tests (snake_case input)
    #[case("user_name", Some("camelCase"), "userName")]
    #[case("first_name", Some("camelCase"), "firstName")]
    #[case("last_name", Some("camelCase"), "lastName")]
    #[case("user_id", Some("camelCase"), "userId")]
    #[case("api_key", Some("camelCase"), "apiKey")]
    #[case("already_camel", Some("camelCase"), "alreadyCamel")]
    // camelCase tests (PascalCase input)
    #[case("UserName", Some("camelCase"), "userName")]
    #[case("UserCreated", Some("camelCase"), "userCreated")]
    #[case("FirstName", Some("camelCase"), "firstName")]
    #[case("ID", Some("camelCase"), "id")]
    #[case("XMLParser", Some("camelCase"), "xmlParser")]
    #[case("HTTPSConnection", Some("camelCase"), "httpsConnection")]
    // snake_case tests
    #[case("userName", Some("snake_case"), "user_name")]
    #[case("firstName", Some("snake_case"), "first_name")]
    #[case("lastName", Some("snake_case"), "last_name")]
    #[case("userId", Some("snake_case"), "user_id")]
    #[case("apiKey", Some("snake_case"), "api_key")]
    #[case("already_snake", Some("snake_case"), "already_snake")]
    // kebab-case tests
    #[case("user_name", Some("kebab-case"), "user-name")]
    #[case("first_name", Some("kebab-case"), "first-name")]
    #[case("last_name", Some("kebab-case"), "last-name")]
    #[case("user_id", Some("kebab-case"), "user-id")]
    #[case("api_key", Some("kebab-case"), "api-key")]
    #[case("already-kebab", Some("kebab-case"), "already-kebab")]
    // PascalCase tests
    #[case("user_name", Some("PascalCase"), "UserName")]
    #[case("first_name", Some("PascalCase"), "FirstName")]
    #[case("last_name", Some("PascalCase"), "LastName")]
    #[case("user_id", Some("PascalCase"), "UserId")]
    #[case("api_key", Some("PascalCase"), "ApiKey")]
    #[case("AlreadyPascal", Some("PascalCase"), "AlreadyPascal")]
    // lowercase tests
    #[case("UserName", Some("lowercase"), "username")]
    #[case("FIRST_NAME", Some("lowercase"), "first_name")]
    #[case("lastName", Some("lowercase"), "lastname")]
    #[case("User_ID", Some("lowercase"), "user_id")]
    #[case("API_KEY", Some("lowercase"), "api_key")]
    #[case("already_lower", Some("lowercase"), "already_lower")]
    // UPPERCASE tests
    #[case("user_name", Some("UPPERCASE"), "USER_NAME")]
    #[case("firstName", Some("UPPERCASE"), "FIRSTNAME")]
    #[case("LastName", Some("UPPERCASE"), "LASTNAME")]
    #[case("user_id", Some("UPPERCASE"), "USER_ID")]
    #[case("apiKey", Some("UPPERCASE"), "APIKEY")]
    #[case("ALREADY_UPPER", Some("UPPERCASE"), "ALREADY_UPPER")]
    // SCREAMING_SNAKE_CASE tests
    #[case("user_name", Some("SCREAMING_SNAKE_CASE"), "USER_NAME")]
    #[case("firstName", Some("SCREAMING_SNAKE_CASE"), "FIRST_NAME")]
    #[case("LastName", Some("SCREAMING_SNAKE_CASE"), "LAST_NAME")]
    #[case("user_id", Some("SCREAMING_SNAKE_CASE"), "USER_ID")]
    #[case("apiKey", Some("SCREAMING_SNAKE_CASE"), "API_KEY")]
    #[case("ALREADY_SCREAMING", Some("SCREAMING_SNAKE_CASE"), "ALREADY_SCREAMING")]
    // SCREAMING-KEBAB-CASE tests
    #[case("user_name", Some("SCREAMING-KEBAB-CASE"), "USER-NAME")]
    #[case("firstName", Some("SCREAMING-KEBAB-CASE"), "FIRST-NAME")]
    #[case("LastName", Some("SCREAMING-KEBAB-CASE"), "LAST-NAME")]
    #[case("user_id", Some("SCREAMING-KEBAB-CASE"), "USER-ID")]
    #[case("apiKey", Some("SCREAMING-KEBAB-CASE"), "API-KEY")]
    #[case("already-kebab", Some("SCREAMING-KEBAB-CASE"), "ALREADY-KEBAB")]
    // None tests (no transformation)
    #[case("user_name", None, "user_name")]
    #[case("firstName", None, "firstName")]
    #[case("LastName", None, "LastName")]
    #[case("user-id", None, "user-id")]
    fn test_rename_field(
        #[case] field_name: &str,
        #[case] rename_all: Option<&str>,
        #[case] expected: &str,
    ) {
        assert_eq!(rename_field(field_name, rename_all), expected);
    }
    // Test camelCase transformation with mixed characters
    #[test]
    fn test_rename_field_camelcase_with_digits() {
        // Tests the regular character branch in camelCase
        let result = rename_field("user_id_123", Some("camelCase"));
        assert_eq!(result, "userId123");

        let result = rename_field("get_user_by_id", Some("camelCase"));
        assert_eq!(result, "getUserById");
    }
    // Test rename_field with unknown/invalid rename_all format - should return original field name
    #[test]
    fn test_rename_field_unknown_format() {
        // Unknown format should return the original field name unchanged
        let result = rename_field("my_field", Some("unknown_format"));
        assert_eq!(result, "my_field");

        let result = rename_field("myField", Some("invalid"));
        assert_eq!(result, "myField");

        let result = rename_field("test_name", Some("not_a_real_format"));
        assert_eq!(result, "test_name");
    }
}
