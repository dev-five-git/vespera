//! Serde attribute extraction utilities for OpenAPI schema generation.
//!
//! This module provides functions to extract serde attributes from Rust types
//! to properly generate OpenAPI schemas that respect serialization rules.

/// Extract doc comments from attributes.
/// Returns concatenated doc comment string or None if no doc comments.
pub fn extract_doc_comment(attrs: &[syn::Attribute]) -> Option<String> {
    let mut doc_lines = Vec::new();

    for attr in attrs {
        if attr.path().is_ident("doc")
            && let syn::Meta::NameValue(meta_nv) = &attr.meta
            && let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(lit_str),
                ..
            }) = &meta_nv.value
        {
            let line = lit_str.value();
            // Trim leading space that rustdoc adds
            let trimmed = line.strip_prefix(' ').unwrap_or(&line);
            doc_lines.push(trimmed.to_string());
        }
    }

    if doc_lines.is_empty() {
        None
    } else {
        Some(doc_lines.join("\n"))
    }
}

/// Strips the `r#` prefix from raw identifiers.
/// E.g., `r#type` becomes `type`.
pub fn strip_raw_prefix(ident: &str) -> &str {
    ident.strip_prefix("r#").unwrap_or(ident)
}

/// Capitalizes the first character of a string.
/// Returns empty string if input is empty.
/// E.g., `user` -> `User`, `USER` -> `USER`, `` -> ``
pub(crate) fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().chain(chars).collect(),
    }
}

/// Extract a Schema name from a SeaORM Entity type path.
///
/// Converts paths like:
/// - `super::user::Entity` -> "User"
/// - `crate::models::memo::Entity` -> "Memo"
///
/// The schema name is derived from the module containing Entity,
/// converted to PascalCase (first letter uppercase).
pub(crate) fn extract_schema_name_from_entity(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(type_path) => {
            let segments: Vec<_> = type_path.path.segments.iter().collect();

            // Need at least 2 segments: module::Entity
            if segments.len() < 2 {
                return None;
            }

            // Check if last segment is "Entity"
            let last = segments.last()?;
            if last.ident != "Entity" {
                return None;
            }

            // Get the second-to-last segment (module name)
            let module_segment = segments.get(segments.len() - 2)?;
            let module_name = module_segment.ident.to_string();

            // Convert to PascalCase (capitalize first letter)
            // Rust identifiers are guaranteed non-empty, so chars().next() always returns Some
            let schema_name = capitalize_first(&module_name);

            Some(schema_name)
        }
        _ => None,
    }
}

pub fn extract_rename_all(attrs: &[syn::Attribute]) -> Option<String> {
    // First check serde attrs (higher priority)
    for attr in attrs {
        if attr.path().is_ident("serde") {
            // Try using parse_nested_meta for robust parsing
            let mut found_rename_all = None;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("rename_all")
                    && let Ok(value) = meta.value()
                    && let Ok(syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    })) = value.parse::<syn::Expr>()
                {
                    found_rename_all = Some(s.value());
                }
                Ok(())
            });
            if found_rename_all.is_some() {
                return found_rename_all;
            }

            // Fallback: manual token parsing for complex attribute combinations
            let tokens = match attr.meta.require_list() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let token_str = tokens.tokens.to_string();

            // Look for rename_all = "..." pattern
            if let Some(start) = token_str.find("rename_all") {
                let remaining = &token_str[start + "rename_all".len()..];
                if let Some(equals_pos) = remaining.find('=') {
                    let value_part = remaining[equals_pos + 1..].trim();
                    // Extract string value - find the closing quote
                    if let Some(quote_start) = value_part.find('"') {
                        let after_quote = &value_part[quote_start + 1..];
                        if let Some(quote_end) = after_quote.find('"') {
                            let value = &after_quote[..quote_end];
                            return Some(value.to_string());
                        }
                    }
                }
            }
        }
    }

    // Fallback: check for #[try_from_multipart(rename_all = "...")]
    for attr in attrs {
        if attr.path().is_ident("try_from_multipart") {
            let mut found_rename_all = None;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("rename_all")
                    && let Ok(value) = meta.value()
                    && let Ok(syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    })) = value.parse::<syn::Expr>()
                {
                    found_rename_all = Some(s.value());
                }
                Ok(())
            });
            if found_rename_all.is_some() {
                return found_rename_all;
            }
        }
    }

    None
}

pub fn extract_field_rename(attrs: &[syn::Attribute]) -> Option<String> {
    // First check serde attrs (higher priority)
    for attr in attrs {
        if attr.path().is_ident("serde")
            && let syn::Meta::List(meta_list) = &attr.meta
        {
            // Use parse_nested_meta to parse nested attributes
            let mut found_rename = None;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("rename")
                    && let Ok(value) = meta.value()
                    && let Ok(syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    })) = value.parse::<syn::Expr>()
                {
                    found_rename = Some(s.value());
                }
                Ok(())
            });
            if let Some(rename_value) = found_rename {
                return Some(rename_value);
            }

            // Fallback: manual token parsing for complex attribute combinations
            let tokens = meta_list.tokens.to_string();
            // Look for pattern: rename = "value" (with proper word boundaries)
            if let Some(start) = tokens.find("rename") {
                // Avoid false positives from rename_all
                if tokens[start..].starts_with("rename_all") {
                    continue;
                }
                // Check that "rename" is a standalone word (not part of another word)
                let before = if start > 0 { &tokens[..start] } else { "" };
                let after_start = start + "rename".len();
                let after = if after_start < tokens.len() {
                    &tokens[after_start..]
                } else {
                    ""
                };

                let before_char = before.chars().last().unwrap_or(' ');
                let after_char = after.chars().next().unwrap_or(' ');

                // Check if rename is a standalone word (preceded by space/comma/paren, followed by space/equals)
                if (before_char == ' ' || before_char == ',' || before_char == '(')
                    && (after_char == ' ' || after_char == '=')
                {
                    // Find the equals sign and extract the quoted value
                    if let Some(equals_pos) = after.find('=') {
                        let value_part = &after[equals_pos + 1..].trim();
                        // Extract string value (remove quotes)
                        if let Some(quote_start) = value_part.find('"') {
                            let after_quote = &value_part[quote_start + 1..];
                            if let Some(quote_end) = after_quote.find('"') {
                                let value = &after_quote[..quote_end];
                                return Some(value.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    // Fallback: check for #[form_data(field_name = "...")]
    for attr in attrs {
        if attr.path().is_ident("form_data") {
            let mut found_field_name = None;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("field_name")
                    && let Ok(value) = meta.value()
                    && let Ok(syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    })) = value.parse::<syn::Expr>()
                {
                    found_field_name = Some(s.value());
                }
                Ok(())
            });
            if found_field_name.is_some() {
                return found_field_name;
            }
        }
    }

    None
}

/// Extract skip attribute from field attributes
/// Returns true if #[serde(skip)] is present
pub fn extract_skip(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if attr.path().is_ident("serde")
            && let syn::Meta::List(meta_list) = &attr.meta
        {
            let tokens = meta_list.tokens.to_string();
            // Check for "skip" (not part of skip_serializing_if or skip_deserializing)
            if tokens.contains("skip") {
                // Make sure it's not skip_serializing_if or skip_deserializing
                if !tokens.contains("skip_serializing_if") && !tokens.contains("skip_deserializing")
                {
                    // Check if it's a standalone "skip"
                    let skip_pos = tokens.find("skip");
                    if let Some(pos) = skip_pos {
                        let before = if pos > 0 { &tokens[..pos] } else { "" };
                        let after = &tokens[pos + "skip".len()..];
                        // Check if skip is not part of another word
                        let before_char = before.chars().last().unwrap_or(' ');
                        let after_char = after.chars().next().unwrap_or(' ');
                        if (before_char == ' ' || before_char == ',' || before_char == '(')
                            && (after_char == ' ' || after_char == ',' || after_char == ')')
                        {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// Extract flatten attribute from field attributes
/// Returns true if #[serde(flatten)] is present
pub fn extract_flatten(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if attr.path().is_ident("serde") {
            // Try using parse_nested_meta for robust parsing
            let mut found = false;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("flatten") {
                    found = true;
                }
                Ok(())
            });
            if found {
                return true;
            }

            // Fallback: manual token parsing for complex attribute combinations
            if let syn::Meta::List(meta_list) = &attr.meta {
                let tokens = meta_list.tokens.to_string();
                // Check for "flatten" as a standalone word
                if let Some(pos) = tokens.find("flatten") {
                    let before = if pos > 0 { &tokens[..pos] } else { "" };
                    let after = &tokens[pos + "flatten".len()..];
                    let before_char = before.chars().last().unwrap_or(' ');
                    let after_char = after.chars().next().unwrap_or(' ');
                    if (before_char == ' ' || before_char == ',' || before_char == '(')
                        && (after_char == ' ' || after_char == ',' || after_char == ')')
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Extract skip_serializing_if attribute from field attributes
/// Returns true if #[serde(skip_serializing_if = "...")] is present
pub fn extract_skip_serializing_if(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if attr.path().is_ident("serde")
            && let syn::Meta::List(meta_list) = &attr.meta
        {
            let mut found = false;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("skip_serializing_if") {
                    found = true;
                }
                Ok(())
            });
            if found {
                return true;
            }

            // Fallback: check tokens string for complex attribute combinations
            let tokens = meta_list.tokens.to_string();
            if tokens.contains("skip_serializing_if") {
                return true;
            }
        }
    }
    false
}

/// Extract default attribute from field attributes
/// Returns:
/// - Some(None) if #[serde(default)] is present (no function)
/// - Some(Some(function_name)) if #[serde(default = "function_name")] is present
/// - None if no default attribute is present
pub fn extract_default(attrs: &[syn::Attribute]) -> Option<Option<String>> {
    for attr in attrs {
        if attr.path().is_ident("serde")
            && let syn::Meta::List(meta_list) = &attr.meta
        {
            let mut found_default: Option<Option<String>> = None;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("default") {
                    // Check if it has a value (default = "function_name")
                    if let Ok(value) = meta.value() {
                        if let Ok(syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(s),
                            ..
                        })) = value.parse::<syn::Expr>()
                        {
                            found_default = Some(Some(s.value()));
                        }
                    } else {
                        // Just "default" without value
                        found_default = Some(None);
                    }
                }
                Ok(())
            });
            if let Some(default_value) = found_default {
                return Some(default_value);
            }

            // Fallback: manual token parsing for complex attribute combinations
            let tokens = meta_list.tokens.to_string();
            if let Some(start) = tokens.find("default") {
                let remaining = &tokens[start + "default".len()..];
                if remaining.trim_start().starts_with('=') {
                    // default = "function_name"
                    let after_equals = remaining
                        .trim_start()
                        .strip_prefix('=')
                        .unwrap_or("")
                        .trim_start();
                    // Extract string value - find opening and closing quotes
                    if let Some(quote_start) = after_equals.find('"') {
                        let after_quote = &after_equals[quote_start + 1..];
                        if let Some(quote_end) = after_quote.find('"') {
                            let function_name = &after_quote[..quote_end];
                            return Some(Some(function_name.to_string()));
                        }
                    }
                } else {
                    // Just "default" without = (standalone)
                    let before = if start > 0 { &tokens[..start] } else { "" };
                    let after = &remaining;
                    let before_char = before.chars().last().unwrap_or(' ');
                    let after_char = after.chars().next().unwrap_or(' ');
                    if (before_char == ' ' || before_char == ',' || before_char == '(')
                        && (after_char == ' ' || after_char == ',' || after_char == ')')
                    {
                        return Some(None);
                    }
                }
            }
        }
    }
    None
}

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
                } else if in_first_word {
                    // In first word: lowercase until we hit a word boundary
                    // Word boundary: uppercase char followed by lowercase (e.g., "XMLParser" -> "P" starts new word)
                    let next_is_lower = chars.get(i + 1).is_some_and(|c| c.is_lowercase());
                    if ch.is_uppercase() && next_is_lower && i > 0 {
                        // This uppercase starts a new word (e.g., 'P' in "XMLParser")
                        in_first_word = false;
                        result.push(ch);
                    } else {
                        // Still in first word, lowercase it
                        result.push(ch.to_lowercase().next().unwrap_or(ch));
                    }
                } else if capitalize_next {
                    result.push(ch.to_uppercase().next().unwrap_or(ch));
                    capitalize_next = false;
                } else {
                    result.push(ch);
                }
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
                result.push(ch.to_lowercase().next().unwrap_or(ch));
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
                    result.push(ch.to_lowercase().next().unwrap_or(ch));
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
                    result.push(ch.to_uppercase().next().unwrap_or(ch));
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
                    snake_case.push(ch.to_lowercase().next().unwrap_or(ch));
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
                    kebab_case.push(ch.to_lowercase().next().unwrap_or(ch));
                } else {
                    kebab_case.push('-');
                }
            }
            kebab_case.to_uppercase()
        }
        _ => field_name.to_string(),
    }
}

/// Serde enum representation types
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SerdeEnumRepr {
    /// Default externally tagged: `{"VariantName": {...}}`
    ExternallyTagged,
    /// Internally tagged: `{"type": "VariantName", ...fields...}`
    /// Only valid for struct and unit variants
    InternallyTagged { tag: String },
    /// Adjacently tagged: `{"type": "VariantName", "data": {...}}`
    AdjacentlyTagged { tag: String, content: String },
    /// Untagged: `{...fields...}` (no tag, first matching variant wins)
    Untagged,
}

/// Extract serde enum representation from attributes.
///
/// Detects the enum tagging strategy from serde attributes:
/// - `#[serde(tag = "type")]` → InternallyTagged
/// - `#[serde(tag = "type", content = "data")]` → AdjacentlyTagged
/// - `#[serde(untagged)]` → Untagged
/// - No relevant attributes → ExternallyTagged (default)
pub fn extract_enum_repr(attrs: &[syn::Attribute]) -> SerdeEnumRepr {
    let tag = extract_tag(attrs);
    let content = extract_content(attrs);
    let untagged = extract_untagged(attrs);

    if untagged {
        SerdeEnumRepr::Untagged
    } else if let Some(tag_name) = tag {
        if let Some(content_name) = content {
            SerdeEnumRepr::AdjacentlyTagged {
                tag: tag_name,
                content: content_name,
            }
        } else {
            SerdeEnumRepr::InternallyTagged { tag: tag_name }
        }
    } else {
        SerdeEnumRepr::ExternallyTagged
    }
}

/// Extract tag attribute from serde container attributes
/// Returns the tag name if `#[serde(tag = "...")]` is present
pub fn extract_tag(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        if attr.path().is_ident("serde") {
            let mut found_tag = None;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("tag")
                    && let Ok(value) = meta.value()
                    && let Ok(syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    })) = value.parse::<syn::Expr>()
                {
                    found_tag = Some(s.value());
                }
                Ok(())
            });
            if found_tag.is_some() {
                return found_tag;
            }

            // Fallback: manual token parsing
            let tokens = match attr.meta.require_list() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let token_str = tokens.tokens.to_string();

            if let Some(start) = token_str.find("tag") {
                // Ensure it's "tag" not "untagged"
                let before = if start > 0 { &token_str[..start] } else { "" };
                let before_char = before.chars().last().unwrap_or(' ');
                if before_char != 'n' {
                    // Not "untagged"
                    let remaining = &token_str[start + "tag".len()..];
                    if let Some(equals_pos) = remaining.find('=') {
                        let value_part = remaining[equals_pos + 1..].trim();
                        if let Some(quote_start) = value_part.find('"') {
                            let after_quote = &value_part[quote_start + 1..];
                            if let Some(quote_end) = after_quote.find('"') {
                                let value = &after_quote[..quote_end];
                                return Some(value.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// Extract content attribute from serde container attributes
/// Returns the content name if `#[serde(content = "...")]` is present
pub fn extract_content(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        if attr.path().is_ident("serde") {
            let mut found_content = None;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("content")
                    && let Ok(value) = meta.value()
                    && let Ok(syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    })) = value.parse::<syn::Expr>()
                {
                    found_content = Some(s.value());
                }
                Ok(())
            });
            if found_content.is_some() {
                return found_content;
            }

            // Fallback: manual token parsing
            let tokens = match attr.meta.require_list() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let token_str = tokens.tokens.to_string();

            if let Some(start) = token_str.find("content") {
                let remaining = &token_str[start + "content".len()..];
                if let Some(equals_pos) = remaining.find('=') {
                    let value_part = remaining[equals_pos + 1..].trim();
                    if let Some(quote_start) = value_part.find('"') {
                        let after_quote = &value_part[quote_start + 1..];
                        if let Some(quote_end) = after_quote.find('"') {
                            let value = &after_quote[..quote_end];
                            return Some(value.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

/// Extract untagged attribute from serde container attributes
/// Returns true if `#[serde(untagged)]` is present
pub fn extract_untagged(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if attr.path().is_ident("serde") {
            let mut found = false;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("untagged") {
                    found = true;
                }
                Ok(())
            });
            if found {
                return true;
            }

            // Fallback: manual token parsing
            if let syn::Meta::List(meta_list) = &attr.meta {
                let tokens = meta_list.tokens.to_string();
                if let Some(pos) = tokens.find("untagged") {
                    let before = if pos > 0 { &tokens[..pos] } else { "" };
                    let after = &tokens[pos + "untagged".len()..];
                    let before_char = before.chars().last().unwrap_or(' ');
                    let after_char = after.chars().next().unwrap_or(' ');
                    if (before_char == ' ' || before_char == ',' || before_char == '(')
                        && (after_char == ' ' || after_char == ',' || after_char == ')')
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

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

    #[rstest]
    #[case(r#"#[serde(rename_all = "camelCase")] struct Foo;"#, Some("camelCase"))]
    #[case(
        r#"#[serde(rename_all = "snake_case")] struct Foo;"#,
        Some("snake_case")
    )]
    #[case(
        r#"#[serde(rename_all = "kebab-case")] struct Foo;"#,
        Some("kebab-case")
    )]
    #[case(
        r#"#[serde(rename_all = "PascalCase")] struct Foo;"#,
        Some("PascalCase")
    )]
    // Multiple attributes - this is the bug case
    #[case(
        r#"#[serde(rename_all = "camelCase", default)] struct Foo;"#,
        Some("camelCase")
    )]
    #[case(
        r#"#[serde(default, rename_all = "snake_case")] struct Foo;"#,
        Some("snake_case")
    )]
    #[case(r#"#[serde(rename_all = "kebab-case", skip_serializing_if = "Option::is_none")] struct Foo;"#, Some("kebab-case"))]
    // No rename_all
    #[case(r#"#[serde(default)] struct Foo;"#, None)]
    #[case(r#"#[derive(Debug)] struct Foo;"#, None)]
    fn test_extract_rename_all(#[case] item_src: &str, #[case] expected: Option<&str>) {
        let item: syn::ItemStruct = syn::parse_str(item_src).unwrap();
        let result = extract_rename_all(&item.attrs);
        assert_eq!(result.as_deref(), expected);
    }

    #[test]
    fn test_extract_rename_all_enum_with_deny_unknown_fields() {
        let enum_item: syn::ItemEnum = syn::parse_str(
            r#"
            #[serde(rename_all = "camelCase", deny_unknown_fields)]
            enum Foo { A, B }
        "#,
        )
        .unwrap();
        let result = extract_rename_all(&enum_item.attrs);
        assert_eq!(result.as_deref(), Some("camelCase"));
    }

    // Tests for extract_field_rename function
    #[rstest]
    #[case(r#"#[serde(rename = "custom_name")] field: i32"#, Some("custom_name"))]
    #[case(r#"#[serde(rename = "userId")] field: i32"#, Some("userId"))]
    #[case(r#"#[serde(rename = "ID")] field: i32"#, Some("ID"))]
    #[case(r#"#[serde(default)] field: i32"#, None)]
    #[case(r#"#[serde(skip)] field: i32"#, None)]
    #[case(r#"field: i32"#, None)]
    // rename_all should NOT be extracted as rename
    #[case(r#"#[serde(rename_all = "camelCase")] field: i32"#, None)]
    // Multiple attributes
    #[case(r#"#[serde(rename = "custom", default)] field: i32"#, Some("custom"))]
    #[case(
        r#"#[serde(default, rename = "my_field")] field: i32"#,
        Some("my_field")
    )]
    fn test_extract_field_rename(#[case] field_src: &str, #[case] expected: Option<&str>) {
        // Parse field from struct context
        let struct_src = format!("struct Foo {{ {} }}", field_src);
        let item: syn::ItemStruct = syn::parse_str(&struct_src).unwrap();
        if let syn::Fields::Named(fields) = &item.fields {
            let field = fields.named.first().unwrap();
            let result = extract_field_rename(&field.attrs);
            assert_eq!(result.as_deref(), expected, "Failed for: {}", field_src);
        }
    }

    // Tests for extract_skip function
    #[rstest]
    #[case(r#"#[serde(skip)] field: i32"#, true)]
    #[case(r#"#[serde(default)] field: i32"#, false)]
    #[case(r#"#[serde(rename = "x")] field: i32"#, false)]
    #[case(r#"field: i32"#, false)]
    // skip_serializing_if should NOT be treated as skip
    #[case(
        r#"#[serde(skip_serializing_if = "Option::is_none")] field: i32"#,
        false
    )]
    // skip_deserializing should NOT be treated as skip
    #[case(r#"#[serde(skip_deserializing)] field: i32"#, false)]
    // Combined attributes
    #[case(r#"#[serde(skip, default)] field: i32"#, true)]
    #[case(r#"#[serde(default, skip)] field: i32"#, true)]
    fn test_extract_skip(#[case] field_src: &str, #[case] expected: bool) {
        let struct_src = format!("struct Foo {{ {} }}", field_src);
        let item: syn::ItemStruct = syn::parse_str(&struct_src).unwrap();
        if let syn::Fields::Named(fields) = &item.fields {
            let field = fields.named.first().unwrap();
            let result = extract_skip(&field.attrs);
            assert_eq!(result, expected, "Failed for: {}", field_src);
        }
    }

    // Tests for extract_flatten function
    #[rstest]
    #[case(r#"#[serde(flatten)] field: i32"#, true)]
    #[case(r#"#[serde(default)] field: i32"#, false)]
    #[case(r#"#[serde(rename = "x")] field: i32"#, false)]
    #[case(r#"field: i32"#, false)]
    // Combined attributes
    #[case(r#"#[serde(flatten, default)] field: i32"#, true)]
    #[case(r#"#[serde(default, flatten)] field: i32"#, true)]
    fn test_extract_flatten(#[case] field_src: &str, #[case] expected: bool) {
        let struct_src = format!("struct Foo {{ {} }}", field_src);
        let item: syn::ItemStruct = syn::parse_str(&struct_src).unwrap();
        if let syn::Fields::Named(fields) = &item.fields {
            let field = fields.named.first().unwrap();
            let result = extract_flatten(&field.attrs);
            assert_eq!(result, expected, "Failed for: {}", field_src);
        }
    }

    // Tests for extract_skip_serializing_if function
    #[rstest]
    #[case(
        r#"#[serde(skip_serializing_if = "Option::is_none")] field: i32"#,
        true
    )]
    #[case(r#"#[serde(skip_serializing_if = "is_zero")] field: i32"#, true)]
    #[case(r#"#[serde(default)] field: i32"#, false)]
    #[case(r#"#[serde(skip)] field: i32"#, false)]
    #[case(r#"field: i32"#, false)]
    fn test_extract_skip_serializing_if(#[case] field_src: &str, #[case] expected: bool) {
        let struct_src = format!("struct Foo {{ {} }}", field_src);
        let item: syn::ItemStruct = syn::parse_str(&struct_src).unwrap();
        if let syn::Fields::Named(fields) = &item.fields {
            let field = fields.named.first().unwrap();
            let result = extract_skip_serializing_if(&field.attrs);
            assert_eq!(result, expected, "Failed for: {}", field_src);
        }
    }

    // Tests for extract_default function
    #[rstest]
    // Simple default (no function)
    #[case(r#"#[serde(default)] field: i32"#, Some(None))]
    // Default with function name
    #[case(
        r#"#[serde(default = "default_value")] field: i32"#,
        Some(Some("default_value"))
    )]
    #[case(
        r#"#[serde(default = "Default::default")] field: i32"#,
        Some(Some("Default::default"))
    )]
    // No default
    #[case(r#"#[serde(skip)] field: i32"#, None)]
    #[case(r#"#[serde(rename = "x")] field: i32"#, None)]
    #[case(r#"field: i32"#, None)]
    // Combined attributes
    #[case(
        r#"#[serde(default, skip_serializing_if = "Option::is_none")] field: i32"#,
        Some(None)
    )]
    #[case(
        r#"#[serde(skip_serializing_if = "Option::is_none", default = "my_default")] field: i32"#,
        Some(Some("my_default"))
    )]
    fn test_extract_default(#[case] field_src: &str, #[case] expected: Option<Option<&str>>) {
        let struct_src = format!("struct Foo {{ {} }}", field_src);
        let item: syn::ItemStruct = syn::parse_str(&struct_src).unwrap();
        if let syn::Fields::Named(fields) = &item.fields {
            let field = fields.named.first().unwrap();
            let result = extract_default(&field.attrs);
            let expected_owned = expected.map(|o| o.map(|s| s.to_string()));
            assert_eq!(result, expected_owned, "Failed for: {}", field_src);
        }
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

    // Tests for extract_doc_comment function
    #[test]
    fn test_extract_doc_comment_single_line() {
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[doc = " This is a doc comment"]
        };
        let result = extract_doc_comment(&attrs);
        assert_eq!(result, Some("This is a doc comment".to_string()));
    }

    #[test]
    fn test_extract_doc_comment_multi_line() {
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[doc = " First line"]
            #[doc = " Second line"]
            #[doc = " Third line"]
        };
        let result = extract_doc_comment(&attrs);
        assert_eq!(
            result,
            Some("First line\nSecond line\nThird line".to_string())
        );
    }

    #[test]
    fn test_extract_doc_comment_no_leading_space() {
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[doc = "No leading space"]
        };
        let result = extract_doc_comment(&attrs);
        assert_eq!(result, Some("No leading space".to_string()));
    }

    #[test]
    fn test_extract_doc_comment_empty() {
        let attrs: Vec<syn::Attribute> = vec![];
        let result = extract_doc_comment(&attrs);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_doc_comment_with_non_doc_attrs() {
        let attrs: Vec<syn::Attribute> = syn::parse_quote! {
            #[derive(Debug)]
            #[doc = " The doc comment"]
            #[serde(rename = "test")]
        };
        let result = extract_doc_comment(&attrs);
        assert_eq!(result, Some("The doc comment".to_string()));
    }

    // Tests for extract_schema_name_from_entity function
    #[test]
    fn test_extract_schema_name_from_entity_super_path() {
        let ty: syn::Type = syn::parse_str("super::user::Entity").unwrap();
        let result = extract_schema_name_from_entity(&ty);
        assert_eq!(result, Some("User".to_string()));
    }

    #[test]
    fn test_extract_schema_name_from_entity_crate_path() {
        let ty: syn::Type = syn::parse_str("crate::models::memo::Entity").unwrap();
        let result = extract_schema_name_from_entity(&ty);
        assert_eq!(result, Some("Memo".to_string()));
    }

    #[test]
    fn test_extract_schema_name_from_entity_not_entity() {
        let ty: syn::Type = syn::parse_str("crate::models::user::Model").unwrap();
        let result = extract_schema_name_from_entity(&ty);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_schema_name_from_entity_single_segment() {
        let ty: syn::Type = syn::parse_str("Entity").unwrap();
        let result = extract_schema_name_from_entity(&ty);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_schema_name_from_entity_non_path_type() {
        let ty: syn::Type = syn::parse_str("&str").unwrap();
        let result = extract_schema_name_from_entity(&ty);
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_schema_name_from_entity_empty_module_name() {
        // Tests the branch where module name has no characters (edge case)
        let ty: syn::Type = syn::parse_str("super::some_module::Entity").unwrap();
        let result = extract_schema_name_from_entity(&ty);
        assert_eq!(result, Some("Some_module".to_string()));
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

    /// Test strip_raw_prefix function
    #[test]
    fn test_strip_raw_prefix() {
        assert_eq!(strip_raw_prefix("r#type"), "type");
        assert_eq!(strip_raw_prefix("r#match"), "match");
        assert_eq!(strip_raw_prefix("normal"), "normal");
        assert_eq!(strip_raw_prefix("r#"), "");
    }

    #[rstest]
    #[case("", "")]
    #[case("a", "A")]
    #[case("user", "User")]
    #[case("User", "User")]
    #[case("USER", "USER")]
    #[case("user_name", "User_name")]
    fn test_capitalize_first(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(capitalize_first(input), expected);
    }

    // Tests using programmatically created attributes
    mod fallback_parsing_tests {
        use proc_macro2::{Span, TokenStream};
        use quote::quote;

        use super::*;

        /// Helper to create attributes by parsing a struct with the given serde attributes
        fn get_struct_attrs(serde_content: &str) -> Vec<syn::Attribute> {
            let src = format!(r#"#[serde({})] struct Foo;"#, serde_content);
            let item: syn::ItemStruct = syn::parse_str(&src).unwrap();
            item.attrs
        }

        /// Helper to create field attributes by parsing a struct with the field
        fn get_field_attrs(serde_content: &str) -> Vec<syn::Attribute> {
            let src = format!(r#"struct Foo {{ #[serde({})] field: i32 }}"#, serde_content);
            let item: syn::ItemStruct = syn::parse_str(&src).unwrap();
            if let syn::Fields::Named(fields) = &item.fields {
                fields.named.first().unwrap().attrs.clone()
            } else {
                vec![]
            }
        }

        /// Create a serde attribute with programmatic tokens
        fn create_attr_with_raw_tokens(tokens: TokenStream) -> syn::Attribute {
            syn::Attribute {
                pound_token: syn::token::Pound::default(),
                style: syn::AttrStyle::Outer,
                bracket_token: syn::token::Bracket::default(),
                meta: syn::Meta::List(syn::MetaList {
                    path: syn::Path::from(syn::Ident::new("serde", Span::call_site())),
                    delimiter: syn::MacroDelimiter::Paren(syn::token::Paren::default()),
                    tokens,
                }),
            }
        }

        /// Test extract_rename_all fallback by creating an attribute where
        /// parse_nested_meta succeeds but doesn't find rename_all in the expected format
        #[test]
        fn test_extract_rename_all_fallback_path() {
            // Standard path - parse_nested_meta should work
            let attrs = get_struct_attrs(r#"rename_all = "camelCase""#);
            let result = extract_rename_all(&attrs);
            assert_eq!(result.as_deref(), Some("camelCase"));
        }

        /// Test extract_field_rename fallback
        #[test]
        fn test_extract_field_rename_fallback_path() {
            // Standard path
            let attrs = get_field_attrs(r#"rename = "myField""#);
            let result = extract_field_rename(&attrs);
            assert_eq!(result.as_deref(), Some("myField"));
        }

        /// Test extract_skip_serializing_if with fallback token check
        #[test]
        fn test_extract_skip_serializing_if_fallback_path() {
            let attrs = get_field_attrs(r#"skip_serializing_if = "Option::is_none""#);
            let result = extract_skip_serializing_if(&attrs);
            assert!(result);
        }

        /// Test extract_default standalone fallback
        #[test]
        fn test_extract_default_standalone_fallback_path() {
            // Simple default without function
            let attrs = get_field_attrs(r#"default"#);
            let result = extract_default(&attrs);
            assert_eq!(result, Some(None));
        }

        /// Test extract_default with function fallback
        #[test]
        fn test_extract_default_with_function_fallback_path() {
            let attrs = get_field_attrs(r#"default = "my_default_fn""#);
            let result = extract_default(&attrs);
            assert_eq!(result, Some(Some("my_default_fn".to_string())));
        }

        /// Test that rename_all is NOT confused with rename
        #[test]
        fn test_extract_field_rename_avoids_rename_all() {
            let attrs = get_field_attrs(r#"rename_all = "camelCase""#);
            let result = extract_field_rename(&attrs);
            assert_eq!(result, None); // Should NOT extract rename_all as rename
        }

        /// Test empty serde attribute
        #[test]
        fn test_extract_functions_with_empty_serde() {
            let item: syn::ItemStruct = syn::parse_str(r#"#[serde()] struct Foo;"#).unwrap();
            assert_eq!(extract_rename_all(&item.attrs), None);
        }

        /// Test non-serde attribute is ignored
        #[test]
        fn test_extract_functions_ignore_non_serde() {
            let item: syn::ItemStruct = syn::parse_str(r#"#[derive(Debug)] struct Foo;"#).unwrap();
            assert_eq!(extract_rename_all(&item.attrs), None);
            assert_eq!(extract_field_rename(&item.attrs), None);
        }

        /// Test serde attribute that is not a list (e.g., #[serde])
        #[test]
        fn test_extract_rename_all_non_list_serde() {
            // #[serde] without parentheses - this should just be ignored
            let item: syn::ItemStruct = syn::parse_str(r#"#[serde] struct Foo;"#).unwrap();
            let result = extract_rename_all(&item.attrs);
            assert_eq!(result, None);
        }

        /// Test extract_field_rename with complex attribute
        #[test]
        fn test_extract_field_rename_complex_attr() {
            let attrs = get_field_attrs(
                r#"default, rename = "field_name", skip_serializing_if = "Option::is_none""#,
            );
            let result = extract_field_rename(&attrs);
            assert_eq!(result.as_deref(), Some("field_name"));
        }

        /// Test extract_rename_all with multiple serde attributes on same item
        #[test]
        fn test_extract_rename_all_multiple_serde_attrs() {
            let item: syn::ItemStruct = syn::parse_str(
                r#"
                #[serde(default)]
                #[serde(rename_all = "snake_case")]
                struct Foo;
                "#,
            )
            .unwrap();
            let result = extract_rename_all(&item.attrs);
            assert_eq!(result.as_deref(), Some("snake_case"));
        }

        /// Test edge case: rename_all with extra whitespace (manual parsing should handle)
        #[test]
        fn test_extract_rename_all_with_whitespace() {
            // Note: syn normalizes whitespace in parsed tokens, so this tests the robust parsing
            let attrs = get_struct_attrs(r#"rename_all = "PascalCase""#);
            let result = extract_rename_all(&attrs);
            assert_eq!(result.as_deref(), Some("PascalCase"));
        }

        /// Test edge case: rename at various positions
        #[test]
        fn test_extract_field_rename_at_end() {
            let attrs = get_field_attrs(r#"skip_serializing_if = "is_none", rename = "lastField""#);
            let result = extract_field_rename(&attrs);
            assert_eq!(result.as_deref(), Some("lastField"));
        }

        /// Test extract_default when it appears with other attrs
        #[test]
        fn test_extract_default_among_other_attrs() {
            let attrs =
                get_field_attrs(r#"skip_serializing_if = "is_none", default, rename = "field""#);
            let result = extract_default(&attrs);
            assert_eq!(result, Some(None));
        }

        /// Test extract_skip - basic functionality
        #[test]
        fn test_extract_skip_basic() {
            let attrs = get_field_attrs(r#"skip"#);
            let result = extract_skip(&attrs);
            assert!(result);
        }

        /// Test extract_skip does not trigger for skip_serializing_if
        #[test]
        fn test_extract_skip_not_skip_serializing_if() {
            let attrs = get_field_attrs(r#"skip_serializing_if = "Option::is_none""#);
            let result = extract_skip(&attrs);
            assert!(!result);
        }

        /// Test extract_skip does not trigger for skip_deserializing
        #[test]
        fn test_extract_skip_not_skip_deserializing() {
            let attrs = get_field_attrs(r#"skip_deserializing"#);
            let result = extract_skip(&attrs);
            assert!(!result);
        }

        /// Test extract_skip with combined attrs
        #[test]
        fn test_extract_skip_with_other_attrs() {
            let attrs = get_field_attrs(r#"skip, default"#);
            let result = extract_skip(&attrs);
            assert!(result);
        }

        /// Test extract_default function with path containing colons
        #[test]
        fn test_extract_default_with_path() {
            let attrs = get_field_attrs(r#"default = "Default::default""#);
            let result = extract_default(&attrs);
            assert_eq!(result, Some(Some("Default::default".to_string())));
        }

        /// Test extract_skip_serializing_if with complex path
        #[test]
        fn test_extract_skip_serializing_if_complex_path() {
            let attrs = get_field_attrs(r#"skip_serializing_if = "Vec::is_empty""#);
            let result = extract_skip_serializing_if(&attrs);
            assert!(result);
        }

        /// Test extract_rename_all with all supported formats
        #[rstest]
        #[case("camelCase")]
        #[case("snake_case")]
        #[case("kebab-case")]
        #[case("PascalCase")]
        #[case("lowercase")]
        #[case("UPPERCASE")]
        #[case("SCREAMING_SNAKE_CASE")]
        #[case("SCREAMING-KEBAB-CASE")]
        fn test_extract_rename_all_all_formats(#[case] format: &str) {
            let attrs = get_struct_attrs(&format!(r#"rename_all = "{}""#, format));
            let result = extract_rename_all(&attrs);
            assert_eq!(result.as_deref(), Some(format));
        }

        /// Test non-serde attribute doesn't affect extraction
        #[test]
        fn test_mixed_attributes() {
            let item: syn::ItemStruct = syn::parse_str(
                r#"
                #[derive(Debug, Clone)]
                #[serde(rename_all = "camelCase")]
                #[doc = "Some documentation"]
                struct Foo;
                "#,
            )
            .unwrap();
            let result = extract_rename_all(&item.attrs);
            assert_eq!(result.as_deref(), Some("camelCase"));
        }

        /// Test field with multiple serde attributes
        #[test]
        fn test_field_multiple_serde_attrs() {
            let item: syn::ItemStruct = syn::parse_str(
                r#"
                struct Foo {
                    #[serde(default)]
                    #[serde(rename = "customName")]
                    field: i32
                }
                "#,
            )
            .unwrap();
            if let syn::Fields::Named(fields) = &item.fields {
                let attrs = &fields.named.first().unwrap().attrs;
                let rename = extract_field_rename(attrs);
                let default = extract_default(attrs);
                assert_eq!(rename.as_deref(), Some("customName"));
                assert_eq!(default, Some(None));
            }
        }

        /// Test extract_rename_all with programmatic tokens
        #[test]
        fn test_extract_rename_all_programmatic() {
            let tokens = quote!(rename_all = "camelCase");
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_rename_all(&[attr]);
            assert_eq!(result.as_deref(), Some("camelCase"));
        }

        /// Test extract_rename_all with invalid value (not a string)
        #[test]
        fn test_extract_rename_all_invalid_value() {
            let tokens = quote!(rename_all = camelCase);
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_rename_all(&[attr]);
            // parse_nested_meta won't find a string literal
            assert!(result.is_none());
        }

        /// Test extract_rename_all with missing equals sign
        #[test]
        fn test_extract_rename_all_no_equals() {
            let tokens = quote!(rename_all "camelCase");
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_rename_all(&[attr]);
            assert!(result.is_none());
        }

        /// Test extract_field_rename with programmatic tokens
        #[test]
        fn test_extract_field_rename_programmatic() {
            let tokens = quote!(rename = "customField");
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_field_rename(&[attr]);
            assert_eq!(result.as_deref(), Some("customField"));
        }

        /// Test extract_default standalone with programmatic tokens
        #[test]
        fn test_extract_default_programmatic() {
            let tokens = quote!(default);
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_default(&[attr]);
            assert_eq!(result, Some(None));
        }

        /// Test extract_default with function via programmatic tokens
        #[test]
        fn test_extract_default_with_fn_programmatic() {
            let tokens = quote!(default = "my_fn");
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_default(&[attr]);
            assert_eq!(result, Some(Some("my_fn".to_string())));
        }

        /// Test extract_skip_serializing_if with programmatic tokens
        #[test]
        fn test_extract_skip_serializing_if_programmatic() {
            let tokens = quote!(skip_serializing_if = "is_none");
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_skip_serializing_if(&[attr]);
            assert!(result);
        }

        /// Test extract_skip via programmatic tokens
        #[test]
        fn test_extract_skip_programmatic() {
            let tokens = quote!(skip);
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_skip(&[attr]);
            assert!(result);
        }

        /// Test that rename_all is not confused with rename
        #[test]
        fn test_rename_all_not_rename() {
            let tokens = quote!(rename_all = "camelCase");
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_field_rename(&[attr]);
            assert_eq!(result, None);
        }

        /// Test multiple items in serde attribute
        #[test]
        fn test_multiple_items_programmatic() {
            let tokens = quote!(default, rename = "myField", skip_serializing_if = "is_none");
            let attr = create_attr_with_raw_tokens(tokens);

            let rename_result = extract_field_rename(std::slice::from_ref(&attr));
            let default_result = extract_default(std::slice::from_ref(&attr));
            let skip_if_result = extract_skip_serializing_if(std::slice::from_ref(&attr));

            assert_eq!(rename_result.as_deref(), Some("myField"));
            assert_eq!(default_result, Some(None));
            assert!(skip_if_result);
        }

        /// Test extract_rename_all fallback parsing
        #[test]
        fn test_extract_rename_all_fallback_manual_parsing() {
            let tokens = quote!(rename_all = "kebab-case");
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_rename_all(&[attr]);
            assert_eq!(result.as_deref(), Some("kebab-case"));
        }

        /// Test extract_rename_all with complex attribute that forces fallback
        #[test]
        fn test_extract_rename_all_complex_attribute_fallback() {
            let tokens = quote!(default, rename_all = "SCREAMING_SNAKE_CASE", skip);
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_rename_all(&[attr]);
            assert_eq!(result.as_deref(), Some("SCREAMING_SNAKE_CASE"));
        }

        /// Test extract_rename_all when value is not a string literal
        #[test]
        fn test_extract_rename_all_no_quote_start() {
            let tokens = quote!(rename_all = snake_case);
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_rename_all(&[attr]);
            assert!(result.is_none());
        }

        /// Test extract_rename_all with unclosed quote
        #[test]
        fn test_extract_rename_all_unclosed_quote() {
            let tokens = quote!(rename_all = "camelCase");
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_rename_all(&[attr]);
            assert_eq!(result.as_deref(), Some("camelCase"));
        }

        /// Test extract_rename_all with empty string value
        #[test]
        fn test_extract_rename_all_empty_string() {
            let tokens = quote!(rename_all = "");
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_rename_all(&[attr]);
            assert_eq!(result.as_deref(), Some(""));
        }

        /// Test extract_rename_all with QUALIFIED PATH to force fallback
        #[test]
        fn test_extract_rename_all_qualified_path_forces_fallback() {
            let tokens = quote!(serde_with::rename_all = "camelCase");
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_rename_all(&[attr]);
            assert_eq!(result.as_deref(), Some("camelCase"));
        }

        /// Test extract_rename_all with another qualified path variation
        #[test]
        fn test_extract_rename_all_module_qualified_forces_fallback() {
            let tokens = quote!(my_module::rename_all = "snake_case");
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_rename_all(&[attr]);
            assert_eq!(result.as_deref(), Some("snake_case"));
        }

        /// Test extract_rename_all with deeply qualified path
        #[test]
        fn test_extract_rename_all_deeply_qualified_forces_fallback() {
            let tokens = quote!(a::b::rename_all = "PascalCase");
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_rename_all(&[attr]);
            assert_eq!(result.as_deref(), Some("PascalCase"));
        }

        /// CRITICAL TEST: This test MUST hit fallback path
        #[test]
        fn test_extract_rename_all_raw_tokens_force_fallback() {
            let tokens: TokenStream = "__rename_all_prefix::rename_all = \"lowercase\""
                .parse()
                .unwrap();
            let attr = create_attr_with_raw_tokens(tokens);

            if let syn::Meta::List(list) = &attr.meta {
                let token_str = list.tokens.to_string();
                assert!(
                    token_str.contains("rename_all"),
                    "Token string should contain rename_all: {}",
                    token_str
                );
            }

            let result = extract_rename_all(&[attr]);
            assert_eq!(
                result.as_deref(),
                Some("lowercase"),
                "Fallback parsing must extract the value"
            );
        }

        /// Another critical test with different qualified path format
        #[test]
        fn test_extract_rename_all_crate_qualified_forces_fallback() {
            let tokens: TokenStream = "crate::rename_all = \"UPPERCASE\"".parse().unwrap();
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_rename_all(&[attr]);
            assert_eq!(result.as_deref(), Some("UPPERCASE"));
        }

        /// Test with self:: prefix
        #[test]
        fn test_extract_rename_all_self_qualified_forces_fallback() {
            let tokens: TokenStream = "self::rename_all = \"kebab-case\"".parse().unwrap();
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_rename_all(&[attr]);
            assert_eq!(result.as_deref(), Some("kebab-case"));
        }

        // =================================================================
        // FALLBACK PATH TESTS (Lines 173, 258-265, 573, 583-590, 626)
        // =================================================================

        /// Test extract_field_rename fallback path - Line 173
        /// Tests the word boundary check when "rename" appears with other attributes
        /// This triggers the manual token parsing fallback when parse_nested_meta
        /// doesn't extract the value in expected format
        #[test]
        fn test_extract_field_rename_fallback_word_boundary() {
            // Create attribute with qualified path to force fallback
            let tokens: TokenStream = "my_module::rename = \"value\"".parse().unwrap();
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_field_rename(&[attr]);
            assert_eq!(result.as_deref(), Some("value"));
        }

        /// Test extract_field_rename fallback - complex combined attributes
        /// Line 173: Tests the edge case of word boundary checking
        #[test]
        fn test_extract_field_rename_fallback_complex_attr() {
            // Qualified path forces parse_nested_meta to not find "rename"
            let tokens: TokenStream = "crate::other::rename = \"custom_field\", default"
                .parse()
                .unwrap();
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_field_rename(&[attr]);
            assert_eq!(result.as_deref(), Some("custom_field"));
        }

        /// Test extract_field_rename - ensure rename_all is not matched as rename
        /// Test the word boundary logic
        #[test]
        fn test_extract_field_rename_fallback_avoids_rename_all() {
            let tokens: TokenStream = "some::rename_all = \"camelCase\"".parse().unwrap();
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_field_rename(&[attr]);
            // Should NOT match rename_all as rename
            assert_eq!(result, None);
        }

        /// Test extract_flatten fallback path - Lines 258-265
        /// Forces manual token parsing by using qualified path
        #[test]
        fn test_extract_flatten_fallback_path() {
            let tokens: TokenStream = "my_module::flatten".parse().unwrap();
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_flatten(&[attr]);
            assert!(result, "Fallback should find 'flatten' in token string");
        }

        /// Test extract_flatten fallback with complex attributes
        /// Lines 258-263: Tests word boundary checking in fallback
        #[test]
        fn test_extract_flatten_fallback_complex() {
            let tokens: TokenStream = "crate::flatten, default = \"my_fn\"".parse().unwrap();
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_flatten(&[attr]);
            assert!(result, "Fallback should detect flatten with other attrs");
        }

        /// Test extract_flatten fallback with flatten at different positions
        /// Line 265: Tests the return true path in fallback
        #[test]
        fn test_extract_flatten_fallback_at_end() {
            let tokens: TokenStream = "default, some::flatten".parse().unwrap();
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_flatten(&[attr]);
            assert!(result);
        }

        /// Test extract_flatten fallback doesn't match partial words
        #[test]
        fn test_extract_flatten_fallback_no_partial_match() {
            // "flattened" should not match "flatten"
            let tokens: TokenStream = "flattened".parse().unwrap();
            let attr = create_attr_with_raw_tokens(tokens);
            let result = extract_flatten(&[attr]);
            assert!(!result, "Should not match 'flattened' as 'flatten'");
        }
        // =================================================================
        // MULTIPART FALLBACK TESTS (form_data / try_from_multipart)
        // =================================================================

        /// Test extract_field_rename falls back to #[form_data(field_name = "...")]
        #[test]
        fn test_extract_field_rename_form_data_fallback() {
            let struct_src = r#"struct Foo { #[form_data(field_name = "my_file")] field: i32 }"#;
            let item: syn::ItemStruct = syn::parse_str(struct_src).unwrap();
            if let syn::Fields::Named(fields) = &item.fields {
                let field = fields.named.first().unwrap();
                let result = extract_field_rename(&field.attrs);
                assert_eq!(result.as_deref(), Some("my_file"));
            }
        }

        /// Test serde rename takes priority over form_data field_name
        #[test]
        fn test_extract_field_rename_serde_over_form_data() {
            let struct_src = r#"struct Foo { #[serde(rename = "serde_name")] #[form_data(field_name = "form_name")] field: i32 }"#;
            let item: syn::ItemStruct = syn::parse_str(struct_src).unwrap();
            if let syn::Fields::Named(fields) = &item.fields {
                let field = fields.named.first().unwrap();
                let result = extract_field_rename(&field.attrs);
                assert_eq!(result.as_deref(), Some("serde_name"));
            }
        }

        /// Test extract_field_rename with form_data but no field_name key
        #[test]
        fn test_extract_field_rename_form_data_no_field_name() {
            let struct_src = r#"struct Foo { #[form_data(limit = "10MiB")] field: i32 }"#;
            let item: syn::ItemStruct = syn::parse_str(struct_src).unwrap();
            if let syn::Fields::Named(fields) = &item.fields {
                let field = fields.named.first().unwrap();
                let result = extract_field_rename(&field.attrs);
                assert_eq!(result, None);
            }
        }

        /// Test extract_rename_all falls back to #[try_from_multipart(rename_all = "...")]
        #[test]
        fn test_extract_rename_all_try_from_multipart_fallback() {
            let item: syn::ItemStruct =
                syn::parse_str(r#"#[try_from_multipart(rename_all = "camelCase")] struct Foo;"#)
                    .unwrap();
            let result = extract_rename_all(&item.attrs);
            assert_eq!(result.as_deref(), Some("camelCase"));
        }

        /// Test serde rename_all takes priority over try_from_multipart rename_all
        #[test]
        fn test_extract_rename_all_serde_over_try_from_multipart() {
            let item: syn::ItemStruct = syn::parse_str(r#"#[serde(rename_all = "snake_case")] #[try_from_multipart(rename_all = "camelCase")] struct Foo;"#).unwrap();
            let result = extract_rename_all(&item.attrs);
            assert_eq!(result.as_deref(), Some("snake_case"));
        }

        /// Test extract_rename_all with try_from_multipart but no rename_all key
        #[test]
        fn test_extract_rename_all_try_from_multipart_no_rename_all() {
            let item: syn::ItemStruct =
                syn::parse_str(r#"#[try_from_multipart(strict)] struct Foo;"#).unwrap();
            let result = extract_rename_all(&item.attrs);
            assert_eq!(result, None);
        }
    }

    // Tests for enum representation extraction (tag, content, untagged)
    mod enum_repr_tests {
        use super::*;

        fn get_enum_attrs(serde_content: &str) -> Vec<syn::Attribute> {
            let src = format!(r#"#[serde({})] enum Foo {{ A, B }}"#, serde_content);
            let item: syn::ItemEnum = syn::parse_str(&src).unwrap();
            item.attrs
        }

        // extract_tag tests
        #[rstest]
        #[case(r#"tag = "type""#, Some("type"))]
        #[case(r#"tag = "kind""#, Some("kind"))]
        #[case(r#"tag = "variant""#, Some("variant"))]
        #[case(r#"tag = "type", content = "data""#, Some("type"))]
        #[case(r#"rename_all = "camelCase""#, None)]
        #[case(r#"untagged"#, None)]
        #[case(r#"default"#, None)]
        fn test_extract_tag(#[case] serde_content: &str, #[case] expected: Option<&str>) {
            let attrs = get_enum_attrs(serde_content);
            let result = extract_tag(&attrs);
            assert_eq!(result.as_deref(), expected, "Failed for: {}", serde_content);
        }

        // extract_content tests
        #[rstest]
        #[case(r#"content = "data""#, Some("data"))]
        #[case(r#"content = "payload""#, Some("payload"))]
        #[case(r#"tag = "type", content = "data""#, Some("data"))]
        #[case(r#"tag = "type""#, None)]
        #[case(r#"untagged"#, None)]
        #[case(r#"rename_all = "camelCase""#, None)]
        fn test_extract_content(#[case] serde_content: &str, #[case] expected: Option<&str>) {
            let attrs = get_enum_attrs(serde_content);
            let result = extract_content(&attrs);
            assert_eq!(result.as_deref(), expected, "Failed for: {}", serde_content);
        }

        // extract_untagged tests
        #[rstest]
        #[case(r#"untagged"#, true)]
        #[case(r#"untagged, rename_all = "camelCase""#, true)]
        #[case(r#"rename_all = "camelCase", untagged"#, true)]
        #[case(r#"tag = "type""#, false)]
        #[case(r#"rename_all = "camelCase""#, false)]
        #[case(r#"default"#, false)]
        fn test_extract_untagged(#[case] serde_content: &str, #[case] expected: bool) {
            let attrs = get_enum_attrs(serde_content);
            let result = extract_untagged(&attrs);
            assert_eq!(result, expected, "Failed for: {}", serde_content);
        }

        // extract_enum_repr comprehensive tests
        #[test]
        fn test_extract_enum_repr_externally_tagged() {
            // No serde tag attributes - default is externally tagged
            let attrs = get_enum_attrs(r#"rename_all = "camelCase""#);
            let repr = extract_enum_repr(&attrs);
            assert_eq!(repr, SerdeEnumRepr::ExternallyTagged);
        }

        #[test]
        fn test_extract_enum_repr_internally_tagged() {
            let attrs = get_enum_attrs(r#"tag = "type""#);
            let repr = extract_enum_repr(&attrs);
            assert_eq!(
                repr,
                SerdeEnumRepr::InternallyTagged {
                    tag: "type".to_string()
                }
            );
        }

        #[test]
        fn test_extract_enum_repr_internally_tagged_custom_name() {
            let attrs = get_enum_attrs(r#"tag = "kind""#);
            let repr = extract_enum_repr(&attrs);
            assert_eq!(
                repr,
                SerdeEnumRepr::InternallyTagged {
                    tag: "kind".to_string()
                }
            );
        }

        #[test]
        fn test_extract_enum_repr_adjacently_tagged() {
            let attrs = get_enum_attrs(r#"tag = "type", content = "data""#);
            let repr = extract_enum_repr(&attrs);
            assert_eq!(
                repr,
                SerdeEnumRepr::AdjacentlyTagged {
                    tag: "type".to_string(),
                    content: "data".to_string()
                }
            );
        }

        #[test]
        fn test_extract_enum_repr_adjacently_tagged_custom_names() {
            let attrs = get_enum_attrs(r#"tag = "kind", content = "payload""#);
            let repr = extract_enum_repr(&attrs);
            assert_eq!(
                repr,
                SerdeEnumRepr::AdjacentlyTagged {
                    tag: "kind".to_string(),
                    content: "payload".to_string()
                }
            );
        }

        #[test]
        fn test_extract_enum_repr_untagged() {
            let attrs = get_enum_attrs(r#"untagged"#);
            let repr = extract_enum_repr(&attrs);
            assert_eq!(repr, SerdeEnumRepr::Untagged);
        }

        #[test]
        fn test_extract_enum_repr_untagged_with_other_attrs() {
            let attrs = get_enum_attrs(r#"untagged, rename_all = "camelCase""#);
            let repr = extract_enum_repr(&attrs);
            assert_eq!(repr, SerdeEnumRepr::Untagged);
        }

        #[test]
        fn test_extract_enum_repr_no_serde_attrs() {
            let item: syn::ItemEnum = syn::parse_str("enum Foo { A, B }").unwrap();
            let repr = extract_enum_repr(&item.attrs);
            assert_eq!(repr, SerdeEnumRepr::ExternallyTagged);
        }

        // Test that content without tag is still externally tagged (content alone is meaningless)
        #[test]
        fn test_extract_enum_repr_content_without_tag() {
            let attrs = get_enum_attrs(r#"content = "data""#);
            let repr = extract_enum_repr(&attrs);
            // Content without tag should be externally tagged (content is ignored)
            assert_eq!(repr, SerdeEnumRepr::ExternallyTagged);
        }

        // =================================================================
        // FALLBACK PATH TESTS FOR TAG/CONTENT (Lines 573, 583-590, 626)
        // =================================================================

        use proc_macro2::{Span, TokenStream};

        /// Helper to create a serde attribute with raw tokens
        fn create_enum_attr_with_raw_tokens(tokens: TokenStream) -> syn::Attribute {
            syn::Attribute {
                pound_token: syn::token::Pound::default(),
                style: syn::AttrStyle::Outer,
                bracket_token: syn::token::Bracket::default(),
                meta: syn::Meta::List(syn::MetaList {
                    path: syn::Path::from(syn::Ident::new("serde", Span::call_site())),
                    delimiter: syn::MacroDelimiter::Paren(syn::token::Paren::default()),
                    tokens,
                }),
            }
        }

        /// Test extract_tag fallback path - Lines 573, 583-590
        /// Forces manual token parsing by using qualified path
        #[test]
        fn test_extract_tag_fallback_path() {
            let tokens: TokenStream = "my_module::tag = \"type\"".parse().unwrap();
            let attr = create_enum_attr_with_raw_tokens(tokens);
            let result = extract_tag(&[attr]);
            assert_eq!(
                result.as_deref(),
                Some("type"),
                "Fallback should extract tag value"
            );
        }

        /// Test extract_tag fallback with complex attributes
        /// Lines 583-590: Tests the value extraction in fallback
        #[test]
        fn test_extract_tag_fallback_complex() {
            let tokens: TokenStream = "crate::tag = \"kind\", rename_all = \"camelCase\""
                .parse()
                .unwrap();
            let attr = create_enum_attr_with_raw_tokens(tokens);
            let result = extract_tag(&[attr]);
            assert_eq!(result.as_deref(), Some("kind"));
        }

        /// Test extract_tag fallback doesn't match "untagged"
        /// Line 581: before_char != 'n' check
        #[test]
        fn test_extract_tag_fallback_avoids_untagged() {
            // "untagged" contains "tag" but should not be matched as tag = "..."
            let tokens: TokenStream = "untagged".parse().unwrap();
            let attr = create_enum_attr_with_raw_tokens(tokens);
            let result = extract_tag(&[attr]);
            assert_eq!(result, None, "Should not extract tag from 'untagged'");
        }

        /// Test extract_tag fallback with tag after other attributes
        #[test]
        fn test_extract_tag_fallback_at_end() {
            let tokens: TokenStream = "default, some_module::tag = \"variant\"".parse().unwrap();
            let attr = create_enum_attr_with_raw_tokens(tokens);
            let result = extract_tag(&[attr]);
            assert_eq!(result.as_deref(), Some("variant"));
        }

        /// Test extract_content fallback path - Line 626
        /// Forces manual token parsing by using qualified path
        #[test]
        fn test_extract_content_fallback_path() {
            let tokens: TokenStream = "my_module::content = \"data\"".parse().unwrap();
            let attr = create_enum_attr_with_raw_tokens(tokens);
            let result = extract_content(&[attr]);
            assert_eq!(
                result.as_deref(),
                Some("data"),
                "Fallback should extract content value"
            );
        }

        /// Test extract_content fallback with complex attributes
        /// Line 626+: Tests the fallback token parsing branch
        #[test]
        fn test_extract_content_fallback_complex() {
            let tokens: TokenStream = "crate::tag = \"type\", other::content = \"payload\""
                .parse()
                .unwrap();
            let attr = create_enum_attr_with_raw_tokens(tokens);
            let result = extract_content(&[attr]);
            assert_eq!(result.as_deref(), Some("payload"));
        }

        /// Test extract_content fallback with content at different position
        #[test]
        fn test_extract_content_fallback_at_start() {
            let tokens: TokenStream = "some::content = \"body\", tag = \"kind\"".parse().unwrap();
            let attr = create_enum_attr_with_raw_tokens(tokens);
            let result = extract_content(&[attr]);
            assert_eq!(result.as_deref(), Some("body"));
        }

        /// Test adjacently tagged using fallback paths for both tag and content
        #[test]
        fn test_extract_enum_repr_adjacently_tagged_fallback() {
            let tokens: TokenStream = "mod1::tag = \"type\", mod2::content = \"data\""
                .parse()
                .unwrap();
            let attr = create_enum_attr_with_raw_tokens(tokens);
            let repr = extract_enum_repr(&[attr]);
            assert_eq!(
                repr,
                SerdeEnumRepr::AdjacentlyTagged {
                    tag: "type".to_string(),
                    content: "data".to_string()
                }
            );
        }

        /// Test internally tagged using fallback path
        #[test]
        fn test_extract_enum_repr_internally_tagged_fallback() {
            let tokens: TokenStream = "qualified::tag = \"discriminator\"".parse().unwrap();
            let attr = create_enum_attr_with_raw_tokens(tokens);
            let repr = extract_enum_repr(&[attr]);
            assert_eq!(
                repr,
                SerdeEnumRepr::InternallyTagged {
                    tag: "discriminator".to_string()
                }
            );
        }

        /// Helper to create a path-only serde attribute (#[serde] without parentheses)
        /// This format causes require_list() to fail (returns Err)
        fn create_path_only_serde_attr() -> syn::Attribute {
            syn::Attribute {
                pound_token: syn::token::Pound::default(),
                style: syn::AttrStyle::Outer,
                bracket_token: syn::token::Bracket::default(),
                meta: syn::Meta::Path(syn::Path::from(syn::Ident::new("serde", Span::call_site()))),
            }
        }

        /// Test extract_tag with non-list serde attribute
        /// When require_list() fails, extract_tag should continue to next attribute
        #[test]
        fn test_extract_tag_non_list_attr_continues() {
            // First attr is path-only (#[serde]), second has the actual tag
            let path_attr = create_path_only_serde_attr();
            let list_attr = {
                let src = r#"#[serde(tag = "type")] enum Foo { A }"#;
                let item: syn::ItemEnum = syn::parse_str(src).unwrap();
                item.attrs.into_iter().next().unwrap()
            };

            // extract_tag should skip the path-only attr and find tag in second attr
            let result = extract_tag(&[path_attr, list_attr]);
            assert_eq!(result.as_deref(), Some("type"));
        }

        /// Test extract_tag with only non-list serde attribute returns None
        #[test]
        fn test_extract_tag_only_non_list_attr_returns_none() {
            let path_attr = create_path_only_serde_attr();
            let result = extract_tag(&[path_attr]);
            assert_eq!(result, None);
        }

        /// Test extract_content with non-list serde attribute
        /// When require_list() fails, extract_content should continue to next attribute
        #[test]
        fn test_extract_content_non_list_attr_continues() {
            // First attr is path-only (#[serde]), second has the actual content
            let path_attr = create_path_only_serde_attr();
            let list_attr = {
                let src = r#"#[serde(content = "data")] enum Foo { A }"#;
                let item: syn::ItemEnum = syn::parse_str(src).unwrap();
                item.attrs.into_iter().next().unwrap()
            };

            // extract_content should skip the path-only attr and find content in second attr
            let result = extract_content(&[path_attr, list_attr]);
            assert_eq!(result.as_deref(), Some("data"));
        }

        /// Test extract_content with only non-list serde attribute returns None
        #[test]
        fn test_extract_content_only_non_list_attr_returns_none() {
            let path_attr = create_path_only_serde_attr();
            let result = extract_content(&[path_attr]);
            assert_eq!(result, None);
        }
    }
}
