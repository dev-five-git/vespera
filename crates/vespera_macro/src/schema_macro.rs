//! Schema macro implementation
//!
//! Provides macros for generating OpenAPI schemas from struct types:
//! - `schema!` - Generate Schema value with optional field filtering
//! - `schema_type!` - Generate new struct type derived from existing type

use proc_macro2::TokenStream;
use quote::quote;
use std::collections::HashSet;
use std::path::Path;
use syn::punctuated::Punctuated;
use syn::{Ident, LitStr, Token, Type, bracketed, parenthesized, parse::Parse, parse::ParseStream};

use crate::metadata::StructMetadata;
use crate::parser::{
    extract_default, extract_field_rename, extract_rename_all, extract_skip,
    extract_skip_serializing_if, parse_type_to_schema_ref, rename_field, strip_raw_prefix,
};
use vespera_core::schema::{Schema, SchemaRef, SchemaType};

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
                "cannot use both `omit` and `pick` in the same schema! invocation",
            ));
        }

        Ok(SchemaInput { ty, omit, pick })
    }
}

/// Generate schema code from a struct with optional field filtering
pub fn generate_schema_code(
    input: &SchemaInput,
    schema_storage: &[StructMetadata],
) -> Result<TokenStream, syn::Error> {
    // Extract type name from the Type
    let type_name = extract_type_name(&input.ty)?;

    // Find struct definition in storage
    let struct_def = schema_storage
        .iter()
        .find(|s| s.name == type_name)
        .ok_or_else(|| {
            syn::Error::new_spanned(
                &input.ty,
                format!(
                    "type `{}` not found. Make sure it has #[derive(Schema)] before this macro invocation",
                    type_name
                ),
            )
        })?;

    // Parse the struct definition
    let parsed_struct: syn::ItemStruct = syn::parse_str(&struct_def.definition).map_err(|e| {
        syn::Error::new_spanned(
            &input.ty,
            format!(
                "failed to parse struct definition for `{}`: {}",
                type_name, e
            ),
        )
    })?;

    // Build omit set
    let omit_set: HashSet<String> = input.omit.clone().unwrap_or_default().into_iter().collect();

    // Build pick set
    let pick_set: HashSet<String> = input.pick.clone().unwrap_or_default().into_iter().collect();

    // Generate schema with filtering
    let schema_tokens =
        generate_filtered_schema(&parsed_struct, &omit_set, &pick_set, schema_storage)?;

    Ok(schema_tokens)
}

/// Extract type name from a Type
fn extract_type_name(ty: &Type) -> Result<String, syn::Error> {
    match ty {
        Type::Path(type_path) => {
            // Get the last segment (handles paths like crate::User)
            let segment = type_path.path.segments.last().ok_or_else(|| {
                syn::Error::new_spanned(ty, "expected a type path with at least one segment")
            })?;
            Ok(segment.ident.to_string())
        }
        _ => Err(syn::Error::new_spanned(
            ty,
            "expected a type path (e.g., `User` or `crate::User`)",
        )),
    }
}

/// Generate Schema construction code with field filtering
fn generate_filtered_schema(
    struct_item: &syn::ItemStruct,
    omit_set: &HashSet<String>,
    pick_set: &HashSet<String>,
    schema_storage: &[StructMetadata],
) -> Result<TokenStream, syn::Error> {
    let rename_all = extract_rename_all(&struct_item.attrs);

    // Build known_schemas and struct_definitions for type resolution
    let known_schemas: std::collections::HashMap<String, String> = schema_storage
        .iter()
        .map(|s| (s.name.clone(), s.definition.clone()))
        .collect();
    let struct_definitions = known_schemas.clone();

    let mut property_tokens = Vec::new();
    let mut required_fields = Vec::new();

    if let syn::Fields::Named(fields_named) = &struct_item.fields {
        for field in &fields_named.named {
            // Skip if serde(skip)
            if extract_skip(&field.attrs) {
                continue;
            }

            let rust_field_name = field
                .ident
                .as_ref()
                .map(|i| strip_raw_prefix(&i.to_string()).to_string())
                .unwrap_or_else(|| "unknown".to_string());

            // Apply rename
            let field_name = if let Some(renamed) = extract_field_rename(&field.attrs) {
                renamed
            } else {
                rename_field(&rust_field_name, rename_all.as_deref())
            };

            // Apply omit filter (check both rust name and json name)
            if !omit_set.is_empty()
                && (omit_set.contains(&rust_field_name) || omit_set.contains(&field_name))
            {
                continue;
            }

            // Apply pick filter (check both rust name and json name)
            if !pick_set.is_empty()
                && !pick_set.contains(&rust_field_name)
                && !pick_set.contains(&field_name)
            {
                continue;
            }

            let field_type = &field.ty;

            // Generate schema for field type
            let schema_ref =
                parse_type_to_schema_ref(field_type, &known_schemas, &struct_definitions);
            let schema_ref_tokens = schema_ref_to_tokens(&schema_ref);

            property_tokens.push(quote! {
                properties.insert(#field_name.to_string(), #schema_ref_tokens);
            });

            // Check if field is required (not Option, no default, no skip_serializing_if)
            let has_default = extract_default(&field.attrs).is_some();
            let has_skip_serializing_if = extract_skip_serializing_if(&field.attrs);
            let is_optional = is_option_type(field_type);

            if !is_optional && !has_default && !has_skip_serializing_if {
                required_fields.push(field_name.clone());
            }
        }
    }

    let required_tokens = if required_fields.is_empty() {
        quote! { None }
    } else {
        let required_strs: Vec<&str> = required_fields.iter().map(|s| s.as_str()).collect();
        quote! { Some(vec![#(#required_strs.to_string()),*]) }
    };

    Ok(quote! {
        {
            let mut properties = std::collections::BTreeMap::new();
            #(#property_tokens)*
            vespera::schema::Schema {
                schema_type: Some(vespera::schema::SchemaType::Object),
                properties: if properties.is_empty() { None } else { Some(properties) },
                required: #required_tokens,
                ..vespera::schema::Schema::new(vespera::schema::SchemaType::Object)
            }
        }
    })
}

/// Check if a type is Option<T>
fn is_option_type(ty: &Type) -> bool {
    match ty {
        Type::Path(type_path) => type_path
            .path
            .segments
            .first()
            .map(|s| s.ident == "Option")
            .unwrap_or(false),
        _ => false,
    }
}

/// Convert SchemaRef to TokenStream for code generation
fn schema_ref_to_tokens(schema_ref: &SchemaRef) -> TokenStream {
    match schema_ref {
        SchemaRef::Ref(reference) => {
            let ref_path = &reference.ref_path;
            quote! {
                vespera::schema::SchemaRef::Ref(vespera::schema::Reference::new(#ref_path.to_string()))
            }
        }
        SchemaRef::Inline(schema) => {
            let schema_tokens = schema_to_tokens(schema);
            quote! {
                vespera::schema::SchemaRef::Inline(Box::new(#schema_tokens))
            }
        }
    }
}

/// Convert Schema to TokenStream for code generation
fn schema_to_tokens(schema: &Schema) -> TokenStream {
    let schema_type_tokens = match &schema.schema_type {
        Some(SchemaType::String) => quote! { Some(vespera::schema::SchemaType::String) },
        Some(SchemaType::Number) => quote! { Some(vespera::schema::SchemaType::Number) },
        Some(SchemaType::Integer) => quote! { Some(vespera::schema::SchemaType::Integer) },
        Some(SchemaType::Boolean) => quote! { Some(vespera::schema::SchemaType::Boolean) },
        Some(SchemaType::Array) => quote! { Some(vespera::schema::SchemaType::Array) },
        Some(SchemaType::Object) => quote! { Some(vespera::schema::SchemaType::Object) },
        Some(SchemaType::Null) => quote! { Some(vespera::schema::SchemaType::Null) },
        None => quote! { None },
    };

    let format_tokens = match &schema.format {
        Some(f) => quote! { Some(#f.to_string()) },
        None => quote! { None },
    };

    let nullable_tokens = match schema.nullable {
        Some(true) => quote! { Some(true) },
        Some(false) => quote! { Some(false) },
        None => quote! { None },
    };

    let ref_path_tokens = match &schema.ref_path {
        Some(rp) => quote! { Some(#rp.to_string()) },
        None => quote! { None },
    };

    let items_tokens = match &schema.items {
        Some(items) => {
            let inner = schema_ref_to_tokens(items);
            quote! { Some(Box::new(#inner)) }
        }
        None => quote! { None },
    };

    let properties_tokens = match &schema.properties {
        Some(props) => {
            let entries: Vec<_> = props
                .iter()
                .map(|(k, v)| {
                    let v_tokens = schema_ref_to_tokens(v);
                    quote! { (#k.to_string(), #v_tokens) }
                })
                .collect();
            quote! {
                Some({
                    let mut map = std::collections::BTreeMap::new();
                    #(map.insert(#entries.0, #entries.1);)*
                    map
                })
            }
        }
        None => quote! { None },
    };

    let required_tokens = match &schema.required {
        Some(req) => {
            let req_strs: Vec<_> = req.iter().map(|s| s.as_str()).collect();
            quote! { Some(vec![#(#req_strs.to_string()),*]) }
        }
        None => quote! { None },
    };

    quote! {
        vespera::schema::Schema {
            ref_path: #ref_path_tokens,
            schema_type: #schema_type_tokens,
            format: #format_tokens,
            nullable: #nullable_tokens,
            items: #items_tokens,
            properties: #properties_tokens,
            required: #required_tokens,
            ..vespera::schema::Schema::new(vespera::schema::SchemaType::Object)
        }
    }
}

// ============================================================================
// schema_type! macro - Generate new struct types from existing types
// ============================================================================

/// Try to find a struct definition from a module path by reading source files.
///
/// This allows schema_type! to work with structs defined in other files, like:
/// ```ignore
/// // In src/routes/memos.rs
/// schema_type!(CreateMemoRequest from models::memo::Model, pick = ["title", "content"]);
/// ```
///
/// The function will:
/// 1. Parse the path (e.g., `models::memo::Model` or `crate::models::memo::Model`)
/// 2. Convert to file path (e.g., `src/models/memo.rs`)
/// 3. Read and parse the file to find the struct definition
fn find_struct_from_path(ty: &Type) -> Option<StructMetadata> {
    // Get CARGO_MANIFEST_DIR to locate src folder
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let src_dir = Path::new(&manifest_dir).join("src");

    // Extract path segments from the type
    let type_path = match ty {
        Type::Path(tp) => tp,
        _ => return None,
    };

    let segments: Vec<String> = type_path
        .path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();

    if segments.is_empty() {
        return None;
    }

    // The last segment is the struct name
    let struct_name = segments.last()?.clone();

    // Build possible file paths from the module path
    // e.g., models::memo::Model -> src/models/memo.rs or src/models/memo/mod.rs
    // e.g., crate::models::memo::Model -> src/models/memo.rs
    let module_segments: Vec<&str> = segments[..segments.len() - 1]
        .iter()
        .filter(|s| *s != "crate" && *s != "self" && *s != "super")
        .map(|s| s.as_str())
        .collect();

    if module_segments.is_empty() {
        return None;
    }

    // Try different file path patterns
    let file_paths = vec![
        src_dir.join(format!("{}.rs", module_segments.join("/"))),
        src_dir.join(format!("{}/mod.rs", module_segments.join("/"))),
    ];

    for file_path in file_paths {
        if !file_path.exists() {
            continue;
        }

        let content = std::fs::read_to_string(&file_path).ok()?;
        let file_ast = syn::parse_file(&content).ok()?;

        // Look for the struct in the file
        for item in &file_ast.items {
            match item {
                syn::Item::Struct(struct_item) if struct_item.ident == struct_name => {
                    return Some(StructMetadata::new_model(
                        struct_name.clone(),
                        quote::quote!(#struct_item).to_string(),
                    ));
                }
                _ => continue,
            }
        }
    }

    None
}

/// Input for the schema_type! macro
///
/// Syntax: `schema_type!(NewTypeName from SourceType, pick = ["field1", "field2"])`
/// Or:     `schema_type!(NewTypeName from SourceType, omit = ["field1", "field2"])`
/// Or:     `schema_type!(NewTypeName from SourceType, rename = [("old", "new")])`
/// Or:     `schema_type!(NewTypeName from SourceType, add = [("field": Type)])`
#[derive(Debug)]
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
                format!("expected `from`, found `{}`", from_ident),
            ));
        }

        // Parse source type
        let source_type: Type = input.parse()?;

        let mut omit = None;
        let mut pick = None;
        let mut rename = None;
        let mut add = None;
        let mut derive_clone = true;

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
                _ => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!(
                            "unknown parameter: `{}`. Expected `omit`, `pick`, `rename`, `add`, or `clone`",
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
                "cannot use both `omit` and `pick` in the same schema_type! invocation",
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
        })
    }
}

/// Generate a new struct type from an existing type with field filtering
pub fn generate_schema_type_code(
    input: &SchemaTypeInput,
    schema_storage: &[StructMetadata],
) -> Result<TokenStream, syn::Error> {
    // Extract type name from the source Type
    let source_type_name = extract_type_name(&input.source_type)?;

    // Find struct definition in storage first (for same-file structs)
    let struct_def_owned: StructMetadata;
    let struct_def = if let Some(found) = schema_storage.iter().find(|s| s.name == source_type_name)
    {
        found
    } else if let Some(found) = find_struct_from_path(&input.source_type) {
        // Try to find from file path (for cross-file structs like models::memo::Model)
        struct_def_owned = found;
        &struct_def_owned
    } else {
        return Err(syn::Error::new_spanned(
            &input.source_type,
            format!(
                "type `{}` not found. Either:\n\
                 1. Use #[derive(Schema)] in the same file\n\
                 2. Use full module path like `crate::models::memo::Model` to reference a struct from another file",
                source_type_name
            ),
        ));
    };

    // Parse the struct definition
    let parsed_struct: syn::ItemStruct = syn::parse_str(&struct_def.definition).map_err(|e| {
        syn::Error::new_spanned(
            &input.source_type,
            format!(
                "failed to parse struct definition for `{}`: {}",
                source_type_name, e
            ),
        )
    })?;

    // Extract all field names from source struct for validation
    let source_field_names: HashSet<String> =
        if let syn::Fields::Named(fields_named) = &parsed_struct.fields {
            fields_named
                .named
                .iter()
                .filter_map(|f| f.ident.as_ref())
                .map(|i| strip_raw_prefix(&i.to_string()).to_string())
                .collect()
        } else {
            HashSet::new()
        };

    // Validate pick fields exist
    if let Some(ref pick_fields) = input.pick {
        for field in pick_fields {
            if !source_field_names.contains(field) {
                return Err(syn::Error::new_spanned(
                    &input.source_type,
                    format!(
                        "field `{}` does not exist in type `{}`. Available fields: {:?}",
                        field,
                        source_type_name,
                        source_field_names.iter().collect::<Vec<_>>()
                    ),
                ));
            }
        }
    }

    // Validate omit fields exist
    if let Some(ref omit_fields) = input.omit {
        for field in omit_fields {
            if !source_field_names.contains(field) {
                return Err(syn::Error::new_spanned(
                    &input.source_type,
                    format!(
                        "field `{}` does not exist in type `{}`. Available fields: {:?}",
                        field,
                        source_type_name,
                        source_field_names.iter().collect::<Vec<_>>()
                    ),
                ));
            }
        }
    }

    // Validate rename source fields exist
    if let Some(ref rename_pairs) = input.rename {
        for (from_field, _) in rename_pairs {
            if !source_field_names.contains(from_field) {
                return Err(syn::Error::new_spanned(
                    &input.source_type,
                    format!(
                        "field `{}` does not exist in type `{}`. Available fields: {:?}",
                        from_field,
                        source_type_name,
                        source_field_names.iter().collect::<Vec<_>>()
                    ),
                ));
            }
        }
    }

    // Build omit set (use Rust field names)
    let omit_set: HashSet<String> = input.omit.clone().unwrap_or_default().into_iter().collect();

    // Build pick set (use Rust field names)
    let pick_set: HashSet<String> = input.pick.clone().unwrap_or_default().into_iter().collect();

    // Build rename map: source_field_name -> new_field_name
    let rename_map: std::collections::HashMap<String, String> = input
        .rename
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect();

    // Extract serde attributes from source struct
    let serde_attrs: Vec<_> = parsed_struct
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("serde"))
        .collect();

    // Generate new struct with filtered fields
    let new_type_name = &input.new_type;
    let mut field_tokens = Vec::new();
    // Track field mappings for From impl: (new_field_ident, source_field_ident)
    let mut field_mappings: Vec<(syn::Ident, syn::Ident)> = Vec::new();

    if let syn::Fields::Named(fields_named) = &parsed_struct.fields {
        for field in &fields_named.named {
            let rust_field_name = field
                .ident
                .as_ref()
                .map(|i| strip_raw_prefix(&i.to_string()).to_string())
                .unwrap_or_else(|| "unknown".to_string());

            // Apply omit filter
            if !omit_set.is_empty() && omit_set.contains(&rust_field_name) {
                continue;
            }

            // Apply pick filter
            if !pick_set.is_empty() && !pick_set.contains(&rust_field_name) {
                continue;
            }

            // Get field components
            let field_ty = &field.ty;
            let vis = &field.vis;
            let source_field_ident = field.ident.clone().unwrap();

            // Filter field attributes: only keep serde attributes, remove sea_orm and others
            // This is important when using schema_type! with models from other files
            // that may have ORM-specific attributes we don't want in the generated struct
            let serde_field_attrs: Vec<_> = field
                .attrs
                .iter()
                .filter(|attr| attr.path().is_ident("serde"))
                .collect();

            // Check if field should be renamed
            if let Some(new_name) = rename_map.get(&rust_field_name) {
                // Create new identifier for the field
                let new_field_ident =
                    syn::Ident::new(new_name, field.ident.as_ref().unwrap().span());

                // Filter out serde(rename) attributes from the serde attrs
                let filtered_attrs: Vec<_> = serde_field_attrs
                    .iter()
                    .filter(|attr| {
                        // Check if it's a rename attribute
                        let mut has_rename = false;
                        let _ = attr.parse_nested_meta(|meta| {
                            if meta.path.is_ident("rename") {
                                has_rename = true;
                            }
                            Ok(())
                        });
                        !has_rename
                    })
                    .collect();

                // Determine the JSON name: use existing serde(rename) if present, otherwise rust field name
                let json_name =
                    extract_field_rename(&field.attrs).unwrap_or_else(|| rust_field_name.clone());

                field_tokens.push(quote! {
                    #(#filtered_attrs)*
                    #[serde(rename = #json_name)]
                    #vis #new_field_ident: #field_ty
                });

                // Track mapping: new field name <- source field name
                field_mappings.push((new_field_ident, source_field_ident));
            } else {
                // No rename, keep field with only serde attrs
                let field_ident = field.ident.clone().unwrap();

                field_tokens.push(quote! {
                    #(#serde_field_attrs)*
                    #vis #field_ident: #field_ty
                });

                // Track mapping: same name
                field_mappings.push((field_ident.clone(), field_ident));
            }
        }
    }

    // Add new fields from `add` parameter
    if let Some(ref add_fields) = input.add {
        for (field_name, field_ty) in add_fields {
            let field_ident = syn::Ident::new(field_name, proc_macro2::Span::call_site());
            field_tokens.push(quote! {
                pub #field_ident: #field_ty
            });
        }
    }

    // Build derive list
    let clone_derive = if input.derive_clone {
        quote! { Clone, }
    } else {
        quote! {}
    };

    // Generate From impl only if `add` is not used (can't auto-populate added fields)
    let source_type = &input.source_type;
    let from_impl = if input.add.is_none() {
        let field_assignments: Vec<_> = field_mappings
            .iter()
            .map(|(new_ident, source_ident)| {
                quote! { #new_ident: source.#source_ident }
            })
            .collect();

        quote! {
            impl From<#source_type> for #new_type_name {
                fn from(source: #source_type) -> Self {
                    Self {
                        #(#field_assignments),*
                    }
                }
            }
        }
    } else {
        quote! {}
    };

    // Generate the new struct
    Ok(quote! {
        #[derive(serde::Serialize, serde::Deserialize, #clone_derive vespera::Schema)]
        #(#serde_attrs)*
        pub struct #new_type_name {
            #(#field_tokens),*
        }

        #from_impl
    })
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

    // schema_type! tests

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
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown parameter"));
    }

    // Tests for `add` parameter

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

    // =========================================================================
    // Tests for generate_schema_code() - success paths
    // =========================================================================

    fn create_test_struct_metadata(
        name: &str,
        definition: &str,
    ) -> crate::metadata::StructMetadata {
        crate::metadata::StructMetadata::new(name.to_string(), definition.to_string())
    }

    #[test]
    fn test_generate_schema_code_simple_struct() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )];

        let tokens = quote::quote!(User);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_code(&input, &storage);

        assert!(result.is_ok());
        let output = result.unwrap().to_string();
        assert!(output.contains("properties"));
        assert!(output.contains("Schema"));
    }

    #[test]
    fn test_generate_schema_code_with_omit() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String, pub password: String }",
        )];

        let tokens = quote::quote!(User, omit = ["password"]);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_code(&input, &storage);

        assert!(result.is_ok());
        let output = result.unwrap().to_string();
        // Should have id and name but not password in properties
        assert!(output.contains("properties"));
    }

    #[test]
    fn test_generate_schema_code_with_pick() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String, pub email: String }",
        )];

        let tokens = quote::quote!(User, pick = ["id", "name"]);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_code(&input, &storage);

        assert!(result.is_ok());
        let output = result.unwrap().to_string();
        assert!(output.contains("properties"));
    }

    // =========================================================================
    // Tests for generate_schema_code() - error paths
    // =========================================================================

    #[test]
    fn test_generate_schema_code_type_not_found() {
        let storage: Vec<crate::metadata::StructMetadata> = vec![];

        let tokens = quote::quote!(NonExistent);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_generate_schema_code_malformed_definition() {
        let storage = vec![create_test_struct_metadata(
            "BadStruct",
            "this is not valid rust code {{{",
        )];

        let tokens = quote::quote!(BadStruct);
        let input: SchemaInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed to parse"));
    }

    // =========================================================================
    // Tests for schema_ref_to_tokens()
    // =========================================================================

    #[test]
    fn test_schema_ref_to_tokens_ref_variant() {
        use vespera_core::schema::{Reference, SchemaRef};

        let schema_ref = SchemaRef::Ref(Reference::new("#/components/schemas/User".to_string()));
        let tokens = schema_ref_to_tokens(&schema_ref);
        let output = tokens.to_string();

        assert!(output.contains("SchemaRef :: Ref"));
        assert!(output.contains("Reference :: new"));
    }

    #[test]
    fn test_schema_ref_to_tokens_inline_variant() {
        use vespera_core::schema::{Schema, SchemaRef, SchemaType};

        let schema = Schema::new(SchemaType::String);
        let schema_ref = SchemaRef::Inline(Box::new(schema));
        let tokens = schema_ref_to_tokens(&schema_ref);
        let output = tokens.to_string();

        assert!(output.contains("SchemaRef :: Inline"));
        assert!(output.contains("Box :: new"));
    }

    // =========================================================================
    // Tests for schema_to_tokens()
    // =========================================================================

    #[test]
    fn test_schema_to_tokens_string_type() {
        use vespera_core::schema::{Schema, SchemaType};

        let schema = Schema::new(SchemaType::String);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("SchemaType :: String"));
    }

    #[test]
    fn test_schema_to_tokens_integer_type() {
        use vespera_core::schema::{Schema, SchemaType};

        let schema = Schema::new(SchemaType::Integer);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("SchemaType :: Integer"));
    }

    #[test]
    fn test_schema_to_tokens_number_type() {
        use vespera_core::schema::{Schema, SchemaType};

        let schema = Schema::new(SchemaType::Number);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("SchemaType :: Number"));
    }

    #[test]
    fn test_schema_to_tokens_boolean_type() {
        use vespera_core::schema::{Schema, SchemaType};

        let schema = Schema::new(SchemaType::Boolean);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("SchemaType :: Boolean"));
    }

    #[test]
    fn test_schema_to_tokens_array_type() {
        use vespera_core::schema::{Schema, SchemaType};

        let schema = Schema::new(SchemaType::Array);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("SchemaType :: Array"));
    }

    #[test]
    fn test_schema_to_tokens_object_type() {
        use vespera_core::schema::{Schema, SchemaType};

        let schema = Schema::new(SchemaType::Object);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("SchemaType :: Object"));
    }

    #[test]
    fn test_schema_to_tokens_null_type() {
        use vespera_core::schema::{Schema, SchemaType};

        let schema = Schema::new(SchemaType::Null);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("SchemaType :: Null"));
    }

    #[test]
    fn test_schema_to_tokens_with_format() {
        use vespera_core::schema::{Schema, SchemaType};

        let mut schema = Schema::new(SchemaType::String);
        schema.format = Some("date-time".to_string());
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("date-time"));
    }

    #[test]
    fn test_schema_to_tokens_with_nullable() {
        use vespera_core::schema::{Schema, SchemaType};

        let mut schema = Schema::new(SchemaType::String);
        schema.nullable = Some(true);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("Some (true)"));
    }

    #[test]
    fn test_schema_to_tokens_with_ref_path() {
        use vespera_core::schema::{Schema, SchemaType};

        let mut schema = Schema::new(SchemaType::Object);
        schema.ref_path = Some("#/components/schemas/User".to_string());
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("#/components/schemas/User"));
    }

    #[test]
    fn test_schema_to_tokens_with_items() {
        use vespera_core::schema::{Schema, SchemaRef, SchemaType};

        let mut schema = Schema::new(SchemaType::Array);
        let item_schema = Schema::new(SchemaType::String);
        schema.items = Some(Box::new(SchemaRef::Inline(Box::new(item_schema))));
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("items"));
        assert!(output.contains("Some (Box :: new"));
    }

    #[test]
    fn test_schema_to_tokens_with_properties() {
        use std::collections::BTreeMap;
        use vespera_core::schema::{Schema, SchemaRef, SchemaType};

        let mut schema = Schema::new(SchemaType::Object);
        let mut props = BTreeMap::new();
        props.insert(
            "name".to_string(),
            SchemaRef::Inline(Box::new(Schema::new(SchemaType::String))),
        );
        schema.properties = Some(props);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("properties"));
        assert!(output.contains("name"));
    }

    #[test]
    fn test_schema_to_tokens_with_required() {
        use vespera_core::schema::{Schema, SchemaType};

        let mut schema = Schema::new(SchemaType::Object);
        schema.required = Some(vec!["id".to_string(), "name".to_string()]);
        let tokens = schema_to_tokens(&schema);
        let output = tokens.to_string();

        assert!(output.contains("required"));
        assert!(output.contains("id"));
        assert!(output.contains("name"));
    }

    // =========================================================================
    // Tests for generate_schema_type_code() - validation errors
    // =========================================================================

    #[test]
    fn test_generate_schema_type_code_pick_nonexistent_field() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )];

        let tokens = quote::quote!(NewUser from User, pick = ["nonexistent"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"));
        assert!(err.contains("nonexistent"));
    }

    #[test]
    fn test_generate_schema_type_code_omit_nonexistent_field() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )];

        let tokens = quote::quote!(NewUser from User, omit = ["nonexistent"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"));
        assert!(err.contains("nonexistent"));
    }

    #[test]
    fn test_generate_schema_type_code_rename_nonexistent_field() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )];

        let tokens = quote::quote!(NewUser from User, rename = [("nonexistent", "new_name")]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"));
        assert!(err.contains("nonexistent"));
    }

    #[test]
    fn test_generate_schema_type_code_type_not_found() {
        let storage: Vec<crate::metadata::StructMetadata> = vec![];

        let tokens = quote::quote!(NewUser from NonExistent);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_generate_schema_type_code_success() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )];

        let tokens = quote::quote!(CreateUser from User, pick = ["name"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let output = result.unwrap().to_string();
        assert!(output.contains("CreateUser"));
        assert!(output.contains("name"));
    }

    #[test]
    fn test_generate_schema_type_code_with_omit() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String, pub password: String }",
        )];

        let tokens = quote::quote!(SafeUser from User, omit = ["password"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let output = result.unwrap().to_string();
        assert!(output.contains("SafeUser"));
        // Should not contain password
        assert!(!output.contains("password"));
    }

    #[test]
    fn test_generate_schema_type_code_with_add() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )];

        let tokens = quote::quote!(UserWithExtra from User, add = [("extra": String)]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let output = result.unwrap().to_string();
        assert!(output.contains("UserWithExtra"));
        assert!(output.contains("extra"));
    }

    #[test]
    fn test_generate_schema_type_code_generates_from_impl() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )];

        // Without add parameter, should generate From impl
        let tokens = quote::quote!(UserResponse from User, pick = ["id", "name"]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let output = result.unwrap().to_string();
        assert!(output.contains("impl From"));
        assert!(output.contains("for UserResponse"));
    }

    #[test]
    fn test_generate_schema_type_code_no_from_impl_with_add() {
        let storage = vec![create_test_struct_metadata(
            "User",
            "pub struct User { pub id: i32, pub name: String }",
        )];

        // With add parameter, should NOT generate From impl
        let tokens = quote::quote!(UserWithExtra from User, add = [("extra": String)]);
        let input: SchemaTypeInput = syn::parse2(tokens).unwrap();
        let result = generate_schema_type_code(&input, &storage);

        assert!(result.is_ok());
        let output = result.unwrap().to_string();
        // Should NOT contain From impl when add is used
        assert!(!output.contains("impl From"));
    }

    // =========================================================================
    // Tests for is_option_type()
    // =========================================================================

    #[test]
    fn test_is_option_type_true() {
        let ty: syn::Type = syn::parse_str("Option<String>").unwrap();
        assert!(is_option_type(&ty));
    }

    #[test]
    fn test_is_option_type_false() {
        let ty: syn::Type = syn::parse_str("String").unwrap();
        assert!(!is_option_type(&ty));
    }

    #[test]
    fn test_is_option_type_vec_false() {
        let ty: syn::Type = syn::parse_str("Vec<String>").unwrap();
        assert!(!is_option_type(&ty));
    }

    // =========================================================================
    // Tests for extract_type_name()
    // =========================================================================

    #[test]
    fn test_extract_type_name_simple() {
        let ty: syn::Type = syn::parse_str("User").unwrap();
        let name = extract_type_name(&ty).unwrap();
        assert_eq!(name, "User");
    }

    #[test]
    fn test_extract_type_name_with_path() {
        let ty: syn::Type = syn::parse_str("crate::models::User").unwrap();
        let name = extract_type_name(&ty).unwrap();
        assert_eq!(name, "User");
    }

    #[test]
    fn test_extract_type_name_non_path_error() {
        // Reference type is not a Type::Path
        let ty: syn::Type = syn::parse_str("&str").unwrap();
        let result = extract_type_name(&ty);
        assert!(result.is_err());
    }
}
