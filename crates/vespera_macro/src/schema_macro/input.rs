//! Input parsing for schema macros
//!
//! Defines input structures for `schema!` and `schema_type!` macros.

use syn::{
    Ident, LitStr, Token, Type, bracketed, parenthesized,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
};

/// Input for the schema! macro
///
/// Supports:
/// - `schema!(Type)` - Full schema
/// - `schema!(Type, omit = ["field1", "field2"])` - Schema with fields omitted
/// - `schema!(Type, pick = ["field1", "field2"])` - Schema with only specified fields (future)
pub struct SchemaInput {
    /// The type to generate schema for
    pub ty: Type,
    /// Fields to omit from the schema
    pub omit: Option<Vec<String>>,
    /// Fields to pick (include only these fields)
    pub pick: Option<Vec<String>>,
}

impl Parse for SchemaInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Parse the type
        let ty: Type = input.parse()?;

        let mut omit = None;
        let mut pick = None;

        // Parse optional parameters
        while input.peek(Token![,]) {
            input.parse::<Token![,]>()?;

            if input.is_empty() {
                break;
            }

            let ident: Ident = input.parse()?;
            let ident_str = ident.to_string();

            match ident_str.as_str() {
                "omit" => {
                    input.parse::<Token![=]>()?;
                    let content;
                    let _ = bracketed!(content in input);
                    let fields: Punctuated<LitStr, Token![,]> =
                        content.parse_terminated(|input| input.parse::<LitStr>(), Token![,])?;
                    omit = Some(fields.into_iter().map(|s| s.value()).collect());
                }
                "pick" => {
                    input.parse::<Token![=]>()?;
                    let content;
                    let _ = bracketed!(content in input);
                    let fields: Punctuated<LitStr, Token![,]> =
                        content.parse_terminated(|input| input.parse::<LitStr>(), Token![,])?;
                    pick = Some(fields.into_iter().map(|s| s.value()).collect());
                }
                _ => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!(
                            "unknown parameter: `{}`. Expected `omit` or `pick`",
                            ident_str
                        ),
                    ));
                }
            }
        }

        // Validate: can't use both omit and pick
        if omit.is_some() && pick.is_some() {
            return Err(syn::Error::new(
                input.span(),
                "schema! macro: cannot use both `omit` and `pick` in the same invocation. Use one or the other to filter fields.",
            ));
        }

        Ok(SchemaInput { ty, omit, pick })
    }
}

/// Input for the schema_type! macro
///
/// Syntax: `schema_type!(NewTypeName from SourceType, pick = ["field1", "field2"])`
/// Or:     `schema_type!(NewTypeName from SourceType, omit = ["field1", "field2"])`
/// Or:     `schema_type!(NewTypeName from SourceType, rename = [("old", "new")])`
/// Or:     `schema_type!(NewTypeName from SourceType, add = [("field": Type)])`
/// Or:     `schema_type!(NewTypeName from SourceType, ignore)` - skip Schema derive
/// Or:     `schema_type!(NewTypeName from SourceType, name = "CustomName")` - custom OpenAPI name
/// Or:     `schema_type!(NewTypeName from SourceType, rename_all = "camelCase")` - serde rename_all
pub struct SchemaTypeInput {
    /// The new type name to generate
    pub new_type: Ident,
    /// The source type to derive from
    pub source_type: Type,
    /// Fields to omit from the new type
    pub omit: Option<Vec<String>>,
    /// Fields to pick (include only these fields)
    pub pick: Option<Vec<String>>,
    /// Field renames: (source_field_name, new_field_name)
    pub rename: Option<Vec<(String, String)>>,
    /// New fields to add: (field_name, field_type)
    pub add: Option<Vec<(String, Type)>>,
    /// Whether to derive Clone (default: true)
    pub derive_clone: bool,
    /// Fields to wrap in `Option<T>` for partial updates.
    ///
    /// - `partial` (bare) = all fields become `Option<T>`
    /// - `partial = ["field1", "field2"]` = only listed fields become `Option<T>`
    /// - Fields already `Option<T>` are left unchanged.
    pub partial: Option<PartialMode>,
    /// Whether to skip deriving the Schema trait (default: false)
    /// Use `ignore` keyword to set this to true.
    pub ignore_schema: bool,
    /// Custom OpenAPI schema name (overrides Rust struct name)
    /// Use `name = "CustomName"` to set this.
    pub schema_name: Option<String>,
    /// Serde rename_all strategy (e.g., "camelCase", "snake_case", "PascalCase")
    /// If not specified, defaults to "camelCase" when source has no rename_all
    pub rename_all: Option<String>,
    /// Whether to generate a multipart/form-data struct (derives TryFromMultipart instead of serde)
    /// Use `multipart` bare keyword to set this to true.
    pub multipart: bool,
}

/// Mode for the `partial` keyword in schema_type!
#[derive(Clone, Debug)]
pub enum PartialMode {
    /// All fields become Option<T>
    All,
    /// Only listed fields become Option<T>
    Fields(Vec<String>),
}

/// Helper struct to parse an add field: ("field_name": Type)
struct AddField {
    name: String,
    ty: Type,
}

impl Parse for AddField {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let content;
        parenthesized!(content in input);
        let name: LitStr = content.parse()?;
        content.parse::<Token![:]>()?;
        let ty: Type = content.parse()?;
        Ok(AddField {
            name: name.value(),
            ty,
        })
    }
}

/// Helper struct to parse a rename pair: ("old_name", "new_name")
struct RenamePair {
    from: String,
    to: String,
}

impl Parse for RenamePair {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let content;
        parenthesized!(content in input);
        let from: LitStr = content.parse()?;
        content.parse::<Token![,]>()?;
        let to: LitStr = content.parse()?;
        Ok(RenamePair {
            from: from.value(),
            to: to.value(),
        })
    }
}

impl Parse for SchemaTypeInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Parse new type name
        let new_type: Ident = input.parse()?;

        // Parse "from" keyword
        let from_ident: Ident = input.parse()?;
        if from_ident != "from" {
            return Err(syn::Error::new(
                from_ident.span(),
                format!(
                    "schema_type! macro: expected `from` keyword, found `{}`. Use format: `schema_type!(NewType from SourceType, ...)`.",
                    from_ident
                ),
            ));
        }

        // Parse source type
        let source_type: Type = input.parse()?;

        let mut omit = None;
        let mut pick = None;
        let mut rename = None;
        let mut add = None;
        let mut derive_clone = true;
        let mut partial = None;
        let mut ignore_schema = false;
        let mut schema_name = None;
        let mut rename_all = None;
        let mut multipart = false;

        // Parse optional parameters
        while input.peek(Token![,]) {
            input.parse::<Token![,]>()?;

            if input.is_empty() {
                break;
            }

            let ident: Ident = input.parse()?;
            let ident_str = ident.to_string();

            match ident_str.as_str() {
                "omit" => {
                    input.parse::<Token![=]>()?;
                    let content;
                    let _ = bracketed!(content in input);
                    let fields: Punctuated<LitStr, Token![,]> =
                        content.parse_terminated(|input| input.parse::<LitStr>(), Token![,])?;
                    omit = Some(fields.into_iter().map(|s| s.value()).collect());
                }
                "pick" => {
                    input.parse::<Token![=]>()?;
                    let content;
                    let _ = bracketed!(content in input);
                    let fields: Punctuated<LitStr, Token![,]> =
                        content.parse_terminated(|input| input.parse::<LitStr>(), Token![,])?;
                    pick = Some(fields.into_iter().map(|s| s.value()).collect());
                }
                "rename" => {
                    input.parse::<Token![=]>()?;
                    let content;
                    let _ = bracketed!(content in input);
                    let pairs: Punctuated<RenamePair, Token![,]> =
                        content.parse_terminated(RenamePair::parse, Token![,])?;
                    rename = Some(pairs.into_iter().map(|p| (p.from, p.to)).collect());
                }
                "add" => {
                    input.parse::<Token![=]>()?;
                    let content;
                    let _ = bracketed!(content in input);
                    let fields: Punctuated<AddField, Token![,]> =
                        content.parse_terminated(AddField::parse, Token![,])?;
                    add = Some(fields.into_iter().map(|f| (f.name, f.ty)).collect());
                }
                "clone" => {
                    input.parse::<Token![=]>()?;
                    let value: syn::LitBool = input.parse()?;
                    derive_clone = value.value();
                }
                "partial" => {
                    if input.peek(Token![=]) {
                        // partial = ["field1", "field2"]
                        input.parse::<Token![=]>()?;
                        let content;
                        let _ = bracketed!(content in input);
                        let fields: Punctuated<LitStr, Token![,]> =
                            content.parse_terminated(|input| input.parse::<LitStr>(), Token![,])?;
                        partial = Some(PartialMode::Fields(
                            fields.into_iter().map(|s| s.value()).collect(),
                        ));
                    } else {
                        // bare `partial` - all fields
                        partial = Some(PartialMode::All);
                    }
                }
                "ignore" => {
                    // bare `ignore` - skip Schema derive
                    ignore_schema = true;
                }
                "name" => {
                    // name = "CustomSchemaName" - custom OpenAPI schema name
                    input.parse::<Token![=]>()?;
                    let name_lit: LitStr = input.parse()?;
                    schema_name = Some(name_lit.value());
                }
                "rename_all" => {
                    // rename_all = "camelCase" - serde rename_all strategy
                    // Validation is delegated to serde at compile time
                    input.parse::<Token![=]>()?;
                    let rename_all_lit: LitStr = input.parse()?;
                    rename_all = Some(rename_all_lit.value());
                }
                "multipart" => {
                    // bare `multipart` - derive TryFromMultipart instead of serde
                    multipart = true;
                }
                _ => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!(
                            "unknown parameter: `{}`. Expected `omit`, `pick`, `rename`, `add`, `clone`, `partial`, `ignore`, `name`, `rename_all`, or `multipart`",
                            ident_str
                        ),
                    ));
                }
            }
        }

        // Validate: can't use both omit and pick
        if omit.is_some() && pick.is_some() {
            return Err(syn::Error::new(
                input.span(),
                "schema_type! macro: cannot use both `omit` and `pick` in the same invocation. Use one or the other to filter fields.",
            ));
        }

        Ok(SchemaTypeInput {
            new_type,
            source_type,
            omit,
            pick,
            rename,
            add,
            derive_clone,
            partial,
            ignore_schema,
            schema_name,
            rename_all,
            multipart,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_schema_input_simple() {
        let tokens = quote::quote!(User);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        assert!(input.omit.is_none());
        assert!(input.pick.is_none());
    }

    #[test]
    fn test_parse_schema_input_with_omit() {
        let tokens = quote::quote!(User, omit = ["password", "secret"]);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let omit = input.omit.unwrap();
        assert_eq!(omit, vec!["password", "secret"]);
    }

    #[test]
    fn test_parse_schema_input_with_pick() {
        let tokens = quote::quote!(User, pick = ["id", "name"]);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let pick = input.pick.unwrap();
        assert_eq!(pick, vec!["id", "name"]);
    }

    #[test]
    fn test_parse_schema_input_omit_and_pick_error() {
        let tokens = quote::quote!(User, omit = ["a"], pick = ["b"]);
        let result: syn::Result<SchemaInput> = syn::parse2(tokens);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_schema_input_trailing_comma() {
        let tokens = quote::quote!(User, omit = ["password"],);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.omit.unwrap(), vec!["password"]);
    }

    #[test]
    fn test_parse_schema_input_unknown_param() {
        let tokens = quote::quote!(User, unknown = ["a"]);
        let result: syn::Result<SchemaInput> = syn::parse2(tokens);
        assert!(result.is_err());
        if let Err(e) = result {
            assert!(e.to_string().contains("unknown parameter"));
        }
    }

    #[test]
    fn test_parse_schema_type_input_simple() {
        let tokens = quote::quote!(CreateUser from User);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.new_type.to_string(), "CreateUser");
        assert!(input.omit.is_none());
        assert!(input.pick.is_none());
        assert!(input.rename.is_none());
        assert!(input.derive_clone);
    }

    #[test]
    fn test_parse_schema_type_input_with_pick() {
        let tokens = quote::quote!(CreateUser from User, pick = ["name", "email"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.new_type.to_string(), "CreateUser");
        let pick = input.pick.unwrap();
        assert_eq!(pick, vec!["name", "email"]);
    }

    #[test]
    fn test_parse_schema_type_input_with_rename() {
        let tokens =
            quote::quote!(UserDTO from User, rename = [("id", "user_id"), ("name", "full_name")]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.new_type.to_string(), "UserDTO");
        let rename = input.rename.unwrap();
        assert_eq!(rename.len(), 2);
        assert_eq!(rename[0], ("id".to_string(), "user_id".to_string()));
        assert_eq!(rename[1], ("name".to_string(), "full_name".to_string()));
    }

    #[test]
    fn test_parse_schema_type_input_with_single_rename() {
        let tokens = quote::quote!(UserDTO from User, rename = [("id", "user_id")]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let rename = input.rename.unwrap();
        assert_eq!(rename.len(), 1);
        assert_eq!(rename[0], ("id".to_string(), "user_id".to_string()));
    }

    #[test]
    fn test_parse_schema_type_input_with_pick_and_rename() {
        let tokens =
            quote::quote!(UserDTO from User, pick = ["id", "name"], rename = [("id", "user_id")]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.pick.unwrap(), vec!["id", "name"]);
        assert_eq!(
            input.rename.unwrap(),
            vec![("id".to_string(), "user_id".to_string())]
        );
    }

    #[test]
    fn test_parse_schema_type_input_with_omit_and_rename() {
        let tokens =
            quote::quote!(UserPublic from User, omit = ["password"], rename = [("id", "user_id")]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.omit.unwrap(), vec!["password"]);
        assert_eq!(
            input.rename.unwrap(),
            vec![("id".to_string(), "user_id".to_string())]
        );
    }

    #[test]
    fn test_parse_schema_type_input_with_clone_false() {
        let tokens = quote::quote!(NonCloneUser from User, clone = false);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert!(!input.derive_clone);
    }

    #[test]
    fn test_parse_schema_type_input_unknown_param_error() {
        let tokens = quote::quote!(UserDTO from User, unknown = ["a"]);
        let result: syn::Result<SchemaTypeInput> = syn::parse2(tokens);
        assert!(result.is_err());
        match result {
            Err(e) => assert!(e.to_string().contains("unknown parameter")),
            Ok(_) => panic!("Expected error"),
        }
    }

    #[test]
    fn test_parse_schema_type_input_with_add_single() {
        let tokens = quote::quote!(UserWithTimestamp from User, add = [("created_at": String)]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.new_type.to_string(), "UserWithTimestamp");
        let add = input.add.unwrap();
        assert_eq!(add.len(), 1);
        assert_eq!(add[0].0, "created_at");
    }

    #[test]
    fn test_parse_schema_type_input_with_add_multiple() {
        let tokens = quote::quote!(UserWithMeta from User, add = [("created_at": String), ("updated_at": Option<String>)]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let add = input.add.unwrap();
        assert_eq!(add.len(), 2);
        assert_eq!(add[0].0, "created_at");
        assert_eq!(add[1].0, "updated_at");
    }

    #[test]
    fn test_parse_schema_type_input_with_pick_and_add() {
        let tokens = quote::quote!(CreateUserWithMeta from User, pick = ["name", "email"], add = [("request_id": String)]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.pick.unwrap(), vec!["name", "email"]);
        let add = input.add.unwrap();
        assert_eq!(add.len(), 1);
        assert_eq!(add[0].0, "request_id");
    }

    #[test]
    fn test_parse_schema_type_input_with_omit_and_add() {
        let tokens = quote::quote!(UserPublicWithMeta from User, omit = ["password"], add = [("display_name": String)]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.omit.unwrap(), vec!["password"]);
        let add = input.add.unwrap();
        assert_eq!(add.len(), 1);
        assert_eq!(add[0].0, "display_name");
    }

    #[test]
    fn test_parse_schema_type_input_with_add_complex_type() {
        let tokens = quote::quote!(UserWithVec from User, add = [("tags": Vec<String>)]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let add = input.add.unwrap();
        assert_eq!(add.len(), 1);
        assert_eq!(add[0].0, "tags");
    }

    #[test]
    fn test_parse_schema_type_input_with_partial_all() {
        let tokens = quote::quote!(UpdateUser from User, partial);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert!(matches!(input.partial, Some(PartialMode::All)));
    }

    #[test]
    fn test_parse_schema_type_input_with_partial_fields() {
        let tokens = quote::quote!(UpdateUser from User, partial = ["name", "email"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        match input.partial {
            Some(PartialMode::Fields(fields)) => {
                assert_eq!(fields, vec!["name", "email"]);
            }
            _ => panic!("Expected PartialMode::Fields"),
        }
    }

    #[test]
    fn test_parse_schema_type_input_with_pick_and_partial() {
        let tokens = quote::quote!(UpdateUser from User, pick = ["name", "email"], partial);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.pick.unwrap(), vec!["name", "email"]);
        assert!(matches!(input.partial, Some(PartialMode::All)));
    }

    #[test]
    fn test_parse_schema_type_input_with_pick_and_partial_fields() {
        let tokens =
            quote::quote!(UpdateUser from User, pick = ["name", "email"], partial = ["name"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.pick.unwrap(), vec!["name", "email"]);
        match input.partial {
            Some(PartialMode::Fields(fields)) => {
                assert_eq!(fields, vec!["name"]);
            }
            _ => panic!("Expected PartialMode::Fields"),
        }
    }

    #[test]
    fn test_parse_schema_type_input_with_ignore() {
        let tokens = quote::quote!(NewType from User, ignore);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert!(input.ignore_schema);
    }

    #[test]
    fn test_parse_schema_type_input_with_name() {
        let tokens = quote::quote!(NewType from User, name = "CustomName");
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.schema_name.as_deref(), Some("CustomName"));
    }

    #[test]
    fn test_parse_schema_type_input_with_name_and_ignore() {
        let tokens = quote::quote!(NewType from User, name = "CustomName", ignore);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.schema_name.as_deref(), Some("CustomName"));
        assert!(input.ignore_schema);
    }

    #[test]
    fn test_parse_schema_type_input_with_rename_all() {
        let tokens = quote::quote!(NewType from User, rename_all = "snake_case");
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.rename_all.as_deref(), Some("snake_case"));
    }

    #[test]
    fn test_parse_schema_type_input_rename_all_with_other_params() {
        let tokens =
            quote::quote!(NewType from User, pick = ["id", "name"], rename_all = "snake_case");
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.pick.unwrap(), vec!["id", "name"]);
        assert_eq!(input.rename_all.as_deref(), Some("snake_case"));
    }

    #[test]
    fn test_parse_schema_type_multiple_commas_trailing() {
        let tokens = quote::quote!(NewType from User, pick = ["id"],);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.pick.unwrap(), vec!["id"]);
    }

    #[test]
    fn test_parse_schema_type_all_parameters() {
        let tokens = quote::quote!(
            NewType from User,
            pick = ["id", "name"],
            rename = [("id", "user_id")],
            clone = false,
            partial,
            name = "CustomName",
            rename_all = "snake_case"
        );
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.pick.unwrap(), vec!["id", "name"]);
        assert!(!input.derive_clone);
        assert!(input.partial.is_some());
        assert_eq!(input.schema_name.as_deref(), Some("CustomName"));
        assert_eq!(input.rename_all.as_deref(), Some("snake_case"));
    }

    // Line 164: Error when "from" keyword is wrong
    #[test]
    fn test_parse_schema_type_input_wrong_from_keyword() {
        let tokens = quote::quote!(NewType xyz User);
        let result: syn::Result<SchemaTypeInput> = syn::parse2(tokens);
        assert!(result.is_err());
        match result {
            Err(e) => assert!(e.to_string().contains("expected `from`"), "Error: {}", e),
            Ok(_) => panic!("Expected error"),
        }
    }

    #[test]
    fn test_parse_schema_type_input_misspelled_from() {
        let tokens = quote::quote!(NewType fron User);
        let result: syn::Result<SchemaTypeInput> = syn::parse2(tokens);
        assert!(result.is_err());
        match result {
            Err(e) => assert!(
                e.to_string()
                    .contains("expected `from` keyword, found `fron`"),
                "Error: {}",
                e
            ),
            Ok(_) => panic!("Expected error"),
        }
    }

    // Line 263: Error when both omit and pick are used
    #[test]
    fn test_parse_schema_type_input_omit_and_pick_error_schema_type() {
        let tokens = quote::quote!(NewType from User, omit = ["a"], pick = ["b"]);
        let result: syn::Result<SchemaTypeInput> = syn::parse2(tokens);
        assert!(result.is_err());
        match result {
            Err(e) => assert!(
                e.to_string().contains("cannot use both `omit` and `pick`"),
                "Error: {}",
                e
            ),
            Ok(_) => panic!("Expected error"),
        }
    }

    #[test]
    fn test_parse_schema_type_input_pick_then_omit_error() {
        // Test the reverse order to ensure both orderings trigger the error
        let tokens = quote::quote!(NewType from User, pick = ["a"], omit = ["b"]);
        let result: syn::Result<SchemaTypeInput> = syn::parse2(tokens);
        assert!(result.is_err());
        match result {
            Err(e) => assert!(
                e.to_string().contains("cannot use both `omit` and `pick`"),
                "Error: {}",
                e
            ),
            Ok(_) => panic!("Expected error"),
        }
    }

    #[test]
    fn test_parse_schema_type_input_with_multipart() {
        let tokens = quote::quote!(UploadReq from CreateUploadRequest, multipart);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert_eq!(input.new_type.to_string(), "UploadReq");
        assert!(input.multipart);
    }

    #[test]
    fn test_parse_schema_type_input_with_multipart_and_pick() {
        let tokens =
            quote::quote!(UploadReq from CreateUploadRequest, multipart, pick = ["name", "file"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert!(input.multipart);
        assert_eq!(input.pick.unwrap(), vec!["name", "file"]);
    }

    #[test]
    fn test_parse_schema_type_input_with_multipart_and_partial() {
        let tokens = quote::quote!(PatchUpload from CreateUploadRequest, multipart, partial);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        assert!(input.multipart);
        assert!(matches!(input.partial, Some(PartialMode::All)));
    }
}
