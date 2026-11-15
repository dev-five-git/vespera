use proc_macro2::TokenStream;
use quote::quote;
use syn::Type;

use super::attrs::{
    extract_belongs_to_from_field, extract_belongs_to_to_field, extract_relation_enum,
    extract_via_rel, is_composite_columns,
};
use crate::schema_macro::module_scope::{Resolved, path_tokens, resolve_name};
use crate::schema_macro::type_utils::{is_option_type, option_inner, seaorm_relation_inner_type};

/// Relation field info for generating `from_model` code.
#[derive(Clone)]
pub struct RelationFieldInfo {
    pub field_name: syn::Ident,
    pub relation_type: String,
    pub schema_path: TokenStream,
    pub is_optional: bool,
    pub inline_type_info: Option<(syn::Ident, Vec<String>)>,
    pub relation_enum: Option<String>,
    pub fk_column: Option<String>,
    /// The `fk_column` field is an `Option` (independent of `is_optional`:
    /// a `BelongsTo<Option<..>>` is optional even with a required key).
    pub fk_optional: bool,
    /// The referenced column on the target (`to = ".."`), when given.
    pub fk_to: Option<String>,
    pub via_rel: Option<String>,
    /// The field is replaced by a `relation_adapters` wrapper
    /// (`From<Option<Model>>`), so `from_model` converts instead of building
    /// the relation schema.
    pub adapter: bool,
}

/// Check if a field in the struct is optional (`Option<T>`). A raw field
/// (`r#ref`) is named by its column (`ref`).
pub fn is_field_optional_in_struct(struct_item: &syn::ItemStruct, field_name: &str) -> bool {
    let field_name = field_name.strip_prefix("r#").unwrap_or(field_name);
    if let syn::Fields::Named(fields_named) = &struct_item.fields {
        for field in &fields_named.named {
            if let Some(ident) = &field.ident
                && syn::ext::IdentExt::unraw(ident) == field_name
            {
                return is_option_type(&field.ty);
            }
        }
    }
    false
}

/// Convert a `SeaORM` relation type to a Schema type AND return relation info.
pub fn convert_relation_type_to_schema_with_info(
    ty: &Type,
    field_attrs: &[syn::Attribute],
    parsed_struct: &syn::ItemStruct,
    source_module_path: &[String],
    field_name: syn::Ident,
) -> Option<(TokenStream, RelationFieldInfo)> {
    let Type::Path(type_path) = ty else {
        return None;
    };

    let segment = type_path.path.segments.last()?;
    let ident_str = segment.ident.to_string();
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first() else {
        return None;
    };
    // SeaORM 2.0's optional `BelongsTo<Option<Entity>>`: the target is the
    // `Option`'s argument, and the relation is optional whatever the FK.
    let (inner_ty, optional_target) =
        option_inner(inner_ty).map_or((inner_ty, false), |target| (target, true));
    let Type::Path(inner_path) = inner_ty else {
        return None;
    };

    let schema_path = schema_path_tokens(&inner_path.path, source_module_path);

    match ident_str.as_str() {
        "HasOne" => Some(single_relation(
            "HasOne",
            field_name,
            field_attrs,
            parsed_struct,
            schema_path,
            optional_target,
        )),
        "HasMany" => {
            let relation_enum = extract_relation_enum(field_attrs);
            let via_rel = extract_via_rel(field_attrs);
            let converted = quote! { Vec<#schema_path> };
            let info = RelationFieldInfo {
                field_name,
                relation_type: "HasMany".to_string(),
                schema_path,
                is_optional: false,
                inline_type_info: None,
                relation_enum,
                fk_column: None,
                fk_optional: true,
                fk_to: None,
                via_rel,
                adapter: false,
            };
            Some((converted, info))
        }
        "BelongsTo" => Some(single_relation(
            "BelongsTo",
            field_name,
            field_attrs,
            parsed_struct,
            schema_path,
            optional_target,
        )),
        _ => None,
    }
}

/// The absolute module (`["crate", .., "user"]`) of the entity a `HasOne` /
/// `HasMany` / `BelongsTo` (or `BelongsTo<Option<..>>`) field written in
/// `module_path` targets; `None` for any other type.
pub fn relation_target_module(ty: &Type, module_path: &[String]) -> Option<Vec<String>> {
    let inner = seaorm_relation_inner_type(ty)?;
    let Type::Path(inner) = option_inner(inner).unwrap_or(inner) else {
        return None;
    };
    let segments: Vec<String> = inner
        .path
        .segments
        .iter()
        .map(|s| crate::parser::strip_raw_prefix_owned(s.ident.to_string()))
        .collect();
    let mut target = absolute_schema_segments(&segments, module_path);
    target.pop();
    Some(target)
}

fn schema_path_tokens(path: &syn::Path, source_module_path: &[String]) -> TokenStream {
    let segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    path_tokens(&absolute_schema_segments(&segments, source_module_path))
}

/// The absolute schema path of a relation's entity path written in the
/// source module: `Entity` / `self::..` name the source module itself (a
/// self-referential relation), `super::..` / `crate::..` are taken
/// literally, a path through an import (`use super::user;` + `user::Entity`)
/// or a child module follows the module scope, and anything else keeps the
/// historical sibling-module reading (`user::Entity` → `<parent>::user`).
fn absolute_schema_segments(segments: &[String], source_module_path: &[String]) -> Vec<String> {
    let first = segments.first().map(String::as_str);
    let resolved = match first {
        Some("super") => {
            let super_count = segments.iter().take_while(|s| *s == "super").count();
            let parent_path_len = source_module_path.len().saturating_sub(super_count);
            source_module_path[..parent_path_len]
                .iter()
                .chain(&segments[super_count..])
                .cloned()
                .collect()
        }
        Some("crate") => segments.to_vec(),
        Some("self") => in_module(source_module_path, &segments[1..]),
        _ if segments.len() == 1 => in_module(source_module_path, segments),
        _ => match resolve_name(segments, source_module_path) {
            Resolved::Path(path) => path,
            Resolved::Unknown => {
                let parent_path_len = source_module_path.len().saturating_sub(1);
                in_module(&source_module_path[..parent_path_len], segments)
            }
        },
    };
    resolved.iter().map(entity_to_schema).collect()
}

fn in_module(module: &[String], rest: &[String]) -> Vec<String> {
    module.iter().chain(rest).cloned().collect()
}

fn entity_to_schema(segment: &String) -> String {
    if segment == "Entity" {
        "Schema".to_string()
    } else {
        segment.clone()
    }
}

fn single_relation(
    relation_type: &str,
    field_name: syn::Ident,
    field_attrs: &[syn::Attribute],
    parsed_struct: &syn::ItemStruct,
    schema_path: TokenStream,
    optional_target: bool,
) -> (TokenStream, RelationFieldInfo) {
    let fk_field = extract_belongs_to_from_field(field_attrs);
    let relation_enum = extract_relation_enum(field_attrs);
    let fk_optional = fk_field
        .as_ref()
        .is_none_or(|f| fk_is_optional(parsed_struct, f));
    let is_optional = optional_target || fk_optional;

    let converted = if is_optional {
        quote! { Option<Box<#schema_path>> }
    } else {
        quote! { Box<#schema_path> }
    };
    let info = RelationFieldInfo {
        field_name,
        relation_type: relation_type.to_string(),
        schema_path,
        is_optional,
        inline_type_info: None,
        relation_enum,
        fk_column: fk_field,
        fk_optional,
        fk_to: extract_belongs_to_to_field(field_attrs),
        // `HasOne` + `via_rel`: the FK is the target's relation of that name.
        via_rel: extract_via_rel(field_attrs),
        adapter: false,
    };
    (converted, info)
}

/// Whether the FK column(s) `fk` may be absent: an `Option` field, or any
/// `Option` column of a composite key.
fn fk_is_optional(parsed_struct: &syn::ItemStruct, fk: &str) -> bool {
    if is_composite_columns(fk) {
        fk.trim_matches(['(', ')'])
            .split(", ")
            .any(|column| is_field_optional_in_struct(parsed_struct, column))
    } else {
        is_field_optional_in_struct(parsed_struct, fk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_struct(def: &str) -> syn::ItemStruct {
        syn::parse_str(def).unwrap()
    }

    fn ident(name: &str) -> syn::Ident {
        syn::Ident::new(name, proc_macro2::Span::call_site())
    }

    #[test]
    fn test_is_field_optional_in_struct_optional() {
        let struct_item = make_test_struct("struct Model { id: i32, user_id: Option<i32> }");
        assert!(is_field_optional_in_struct(&struct_item, "user_id"));
    }

    #[test]
    fn test_is_field_optional_in_struct_required() {
        let struct_item = make_test_struct("struct Model { id: i32, user_id: i32 }");
        assert!(!is_field_optional_in_struct(&struct_item, "user_id"));
    }

    /// A keyword column is the raw field of that name.
    #[test]
    fn test_is_field_optional_in_struct_raw_identifier() {
        let struct_item = make_test_struct("struct Model { id: i32, r#ref: Option<i32> }");
        assert!(is_field_optional_in_struct(&struct_item, "ref"));
        assert!(is_field_optional_in_struct(&struct_item, "r#ref"));
    }

    #[test]
    fn test_is_field_optional_in_struct_field_not_found() {
        let struct_item = make_test_struct("struct Model { id: i32 }");
        assert!(!is_field_optional_in_struct(&struct_item, "nonexistent"));
    }

    #[test]
    fn test_is_field_optional_in_struct_tuple_struct() {
        let struct_item: syn::ItemStruct =
            syn::parse_str("struct TupleStruct(i32, Option<String>);").unwrap();
        assert!(!is_field_optional_in_struct(&struct_item, "0"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_non_path_type() {
        let ty: syn::Type = syn::parse_str("&str").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        assert!(
            convert_relation_type_to_schema_with_info(&ty, &[], &struct_item, &[], ident("user"))
                .is_none()
        );
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_empty_segments() {
        let ty = syn::Type::Path(syn::TypePath {
            attrs: Vec::new(),
            qself: None,
            path: syn::Path {
                leading_colon: None,
                segments: syn::punctuated::Punctuated::new(),
            },
        });
        let struct_item = make_test_struct("struct Model { id: i32 }");
        assert!(
            convert_relation_type_to_schema_with_info(&ty, &[], &struct_item, &[], ident("user"))
                .is_none()
        );
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_no_angle_brackets() {
        let ty: syn::Type = syn::parse_str("HasOne").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        assert!(
            convert_relation_type_to_schema_with_info(&ty, &[], &struct_item, &[], ident("user"))
                .is_none()
        );
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_non_type_generic() {
        let ty: syn::Type = syn::parse_str("HasOne<'a>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        assert!(
            convert_relation_type_to_schema_with_info(&ty, &[], &struct_item, &[], ident("user"))
                .is_none()
        );
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_non_path_inner() {
        let ty: syn::Type = syn::parse_str("HasOne<&str>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        assert!(
            convert_relation_type_to_schema_with_info(&ty, &[], &struct_item, &[], ident("user"))
                .is_none()
        );
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_has_one_optional() {
        let ty: syn::Type = syn::parse_str("HasOne<user::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32, user_id: Option<i32> }");
        let attrs = vec![syn::parse_quote!(#[sea_orm(belongs_to, from = "user_id")])];
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        let (tokens, info) = convert_relation_type_to_schema_with_info(
            &ty,
            &attrs,
            &struct_item,
            &module_path,
            ident("user"),
        )
        .unwrap();
        assert_eq!(info.relation_type, "HasOne");
        assert!(info.is_optional);
        assert!(tokens.to_string().contains("Option"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_has_one_required() {
        let ty: syn::Type = syn::parse_str("HasOne<user::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32, user_id: i32 }");
        let attrs = vec![syn::parse_quote!(#[sea_orm(belongs_to, from = "user_id")])];
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        let (tokens, info) = convert_relation_type_to_schema_with_info(
            &ty,
            &attrs,
            &struct_item,
            &module_path,
            ident("user"),
        )
        .unwrap();
        assert_eq!(info.relation_type, "HasOne");
        assert!(!info.is_optional);
        assert!(tokens.to_string().contains("Box"));
        assert!(!tokens.to_string().contains("Option"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_has_one_no_fk() {
        let ty: syn::Type = syn::parse_str("HasOne<user::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        let (tokens, info) = convert_relation_type_to_schema_with_info(
            &ty,
            &[],
            &struct_item,
            &module_path,
            ident("user"),
        )
        .unwrap();
        assert!(info.is_optional);
        assert!(tokens.to_string().contains("Option"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_has_many() {
        let ty: syn::Type = syn::parse_str("HasMany<memo::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];
        let (tokens, info) = convert_relation_type_to_schema_with_info(
            &ty,
            &[],
            &struct_item,
            &module_path,
            ident("memos"),
        )
        .unwrap();
        assert_eq!(info.relation_type, "HasMany");
        assert!(!info.is_optional);
        assert!(tokens.to_string().contains("Vec"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_belongs_to_optional() {
        let ty: syn::Type = syn::parse_str("BelongsTo<user::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32, user_id: Option<i32> }");
        let attrs = vec![syn::parse_quote!(#[sea_orm(belongs_to, from = "user_id")])];
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        let (tokens, info) = convert_relation_type_to_schema_with_info(
            &ty,
            &attrs,
            &struct_item,
            &module_path,
            ident("user"),
        )
        .unwrap();
        assert_eq!(info.relation_type, "BelongsTo");
        assert!(info.is_optional);
        assert!(tokens.to_string().contains("Option"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_belongs_to_required() {
        let ty: syn::Type = syn::parse_str("BelongsTo<user::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32, user_id: i32 }");
        let attrs = vec![syn::parse_quote!(#[sea_orm(belongs_to, from = "user_id")])];
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        let (tokens, info) = convert_relation_type_to_schema_with_info(
            &ty,
            &attrs,
            &struct_item,
            &module_path,
            ident("user"),
        )
        .unwrap();
        assert_eq!(info.relation_type, "BelongsTo");
        assert!(!info.is_optional);
        assert!(!tokens.to_string().contains("Option"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_unknown_relation() {
        let ty: syn::Type = syn::parse_str("SomeOtherType<user::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        assert!(
            convert_relation_type_to_schema_with_info(&ty, &[], &struct_item, &[], ident("user"))
                .is_none()
        );
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_super_path() {
        let ty: syn::Type = syn::parse_str("HasMany<super::memo::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];
        let (tokens, _) = convert_relation_type_to_schema_with_info(
            &ty,
            &[],
            &struct_item,
            &module_path,
            ident("memos"),
        )
        .unwrap();
        let output = tokens.to_string();
        assert!(output.contains("crate"));
        assert!(output.contains("models"));
        assert!(output.contains("memo"));
        assert!(output.contains("Schema"));
    }

    #[test]
    fn relation_path_resolves_multiple_super_segments() {
        let ty: syn::Type = syn::parse_str("HasMany<super::super::user::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "nested".to_string(),
            "memo".to_string(),
        ];

        let (tokens, info) = convert_relation_type_to_schema_with_info(
            &ty,
            &[],
            &struct_item,
            &module_path,
            ident("users"),
        )
        .unwrap();

        assert_eq!(
            tokens.to_string(),
            "Vec < crate :: models :: user :: Schema >"
        );
        assert_eq!(
            info.schema_path.to_string(),
            "crate :: models :: user :: Schema"
        );
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_crate_path() {
        let ty: syn::Type = syn::parse_str("HasMany<crate::models::memo::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "user".to_string(),
        ];
        let (tokens, _) = convert_relation_type_to_schema_with_info(
            &ty,
            &[],
            &struct_item,
            &module_path,
            ident("memos"),
        )
        .unwrap();
        let output = tokens.to_string();
        assert!(output.contains("crate"));
        assert!(output.contains("models"));
        assert!(output.contains("memo"));
        assert!(output.contains("Schema"));
        assert!(!output.contains("Entity"));
    }

    #[test]
    fn test_convert_relation_type_to_schema_with_info_relative_path() {
        let ty: syn::Type = syn::parse_str("HasOne<user::Entity>").unwrap();
        let struct_item = make_test_struct("struct Model { id: i32 }");
        let module_path = vec![
            "crate".to_string(),
            "models".to_string(),
            "memo".to_string(),
        ];
        let (tokens, _) = convert_relation_type_to_schema_with_info(
            &ty,
            &[],
            &struct_item,
            &module_path,
            ident("user"),
        )
        .unwrap();
        let output = tokens.to_string();
        assert!(output.contains("crate"));
        assert!(output.contains("models"));
        assert!(output.contains("user"));
        assert!(output.contains("Schema"));
    }

    fn segs(path: &str) -> Vec<String> {
        path.split("::").map(str::to_string).collect()
    }

    #[rstest::rstest]
    #[case::self_referential("Entity", "crate::models::category::Schema")]
    #[case::self_prefixed("self::Entity", "crate::models::category::Schema")]
    #[case::super_sibling("super::user::Entity", "crate::models::user::Schema")]
    #[case::crate_absolute("crate::other::Entity", "crate::other::Schema")]
    #[case::imported_module("user::Entity", "crate::models::user::Schema")]
    #[case::child_module("nested::Entity", "crate::models::category::nested::Schema")]
    #[case::unknown_sibling("tag::Entity", "crate::models::tag::Schema")]
    fn relation_paths_resolve_in_source_module(#[case] path: &str, #[case] expected: &str) {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("src/models/category.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "use super::user;\npub mod nested {}\n").unwrap();
        let _guard = crate::env_access::test_env::set_var("CARGO_MANIFEST_DIR", dir.path());
        crate::schema_macro::file_cache::bump_epoch();
        let module = segs("crate::models::category");
        assert_eq!(
            absolute_schema_segments(&segs(path), &module).join("::"),
            expected
        );
    }

    // SeaORM 2.0 shapes: `BelongsTo<Option<E>>` targets `E` (it used to
    // build `cake::Option`) and is optional even with a required key;
    // `from` / `to` column paths name fields; `HasOne` keeps `via_rel`; a
    // composite key is optional when any of its columns is.
    #[rstest::rstest]
    #[case::optional_belongs_to(
        r#"#[sea_orm(belongs_to, from = "bakery_id", to = "id")] pub bakery: BelongsTo<Option<super::bakery::Entity>>"#,
        "Option < Box < crate :: models :: bakery :: Schema >>",
        (true, false),
        (Some("bakery_id"), Some("id"), None)
    )]
    #[case::column_paths(
        r#"#[sea_orm(belongs_to, from = "Column::BakeryId", to = "Id")] pub bakery: BelongsTo<super::bakery::Entity>"#,
        "Box < crate :: models :: bakery :: Schema >",
        (false, false),
        (Some("bakery_id"), Some("id"), None)
    )]
    #[case::has_one_via_rel(
        r#"#[sea_orm(has_one, relation_enum = "Profile", via_rel = "Owner")] pub profile: HasOne<super::profile::Entity>"#,
        "Option < Box < crate :: models :: profile :: Schema >>",
        (true, true),
        (None, None, Some("Owner"))
    )]
    #[case::composite_key(
        r#"#[sea_orm(belongs_to, from = "(BakeryId, tenant_id)", to = "(Id, TenantId)")] pub bakery: BelongsTo<super::bakery::Entity>"#,
        "Option < Box < crate :: models :: bakery :: Schema >>",
        (true, true),
        (Some("(bakery_id, tenant_id)"), Some("(id, tenant_id)"), None)
    )]
    fn seaorm_2_relation_shapes(
        #[case] field: &str,
        #[case] converted: &str,
        #[case] (is_optional, fk_optional): (bool, bool),
        #[case] (fk_column, fk_to, via_rel): (Option<&str>, Option<&str>, Option<&str>),
    ) {
        let parsed = make_test_struct(&format!(
            "pub struct Model {{ pub id: i32, pub bakery_id: i32, pub tenant_id: Option<i32>, {field} }}"
        ));
        let relation = parsed.fields.iter().next_back().unwrap();
        let (tokens, info) = convert_relation_type_to_schema_with_info(
            &relation.ty,
            &relation.attrs,
            &parsed,
            &segs("crate::models::cake"),
            relation.ident.clone().unwrap(),
        )
        .unwrap();
        assert_eq!(tokens.to_string(), converted);
        assert_eq!(
            (info.is_optional, info.fk_optional),
            (is_optional, fk_optional)
        );
        assert_eq!(
            (
                info.fk_column.as_deref(),
                info.fk_to.as_deref(),
                info.via_rel.as_deref()
            ),
            (fk_column, fk_to, via_rel)
        );
    }

    #[rstest::rstest]
    #[case::optional_target(
        "BelongsTo<Option<super::bakery::Entity>>",
        Some("crate::models::bakery")
    )]
    #[case::raw_module("HasOne<super::r#type::Entity>", Some("crate::models::type"))]
    #[case::not_a_relation("Option<super::bakery::Entity>", None)]
    #[case::non_path_target("HasOne<&'static str>", None)]
    fn relation_target_modules(#[case] ty: &str, #[case] expected: Option<&str>) {
        let ty: syn::Type = syn::parse_str(ty).unwrap();
        assert_eq!(
            relation_target_module(&ty, &segs("crate::models::cake")),
            expected.map(segs)
        );
    }

    #[test]
    fn raw_identifier_relation_paths_render() {
        let path: syn::Path = syn::parse_str("crate::r#type::Entity").unwrap();
        assert_eq!(
            schema_path_tokens(&path, &[]).to_string(),
            "crate :: r#type :: Schema"
        );
    }
}
