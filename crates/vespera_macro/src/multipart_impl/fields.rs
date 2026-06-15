use proc_macro2::TokenStream;
use quote::quote;
use syn::Type;

use super::attrs::{extract_limit_tokens, resolve_default_kind, resolve_field_name};
use super::types::{extract_inner_generic, is_option_type, is_vec_type};

/// Collected codegen fragments for each struct field.
pub(super) struct FieldCodegen<'a> {
    pub(super) declarations: Vec<TokenStream>,
    pub(super) assignments: Vec<TokenStream>,
    pub(super) post_loop: Vec<TokenStream>,
    pub(super) idents: Vec<&'a syn::Ident>,
}

/// How a missing field should be handled.
pub(super) enum DefaultKind {
    /// No default — field is required; emit `MissingField` error.
    None,
    /// Use `Default::default()` — from `#[serde(default)]` or `#[form_data(default)]`.
    Trait,
    /// Call a custom function — from `#[serde(default = "path::to::fn")]`.
    Function(String),
}

/// Process all named fields into codegen fragments.
pub(super) fn process_fields<'a>(
    fields: impl Iterator<Item = &'a syn::Field>,
    rename_all: Option<&str>,
    strict: bool,
    struct_default: bool,
) -> FieldCodegen<'a> {
    let mut cg = FieldCodegen {
        declarations: Vec::new(),
        assignments: Vec::new(),
        post_loop: Vec::new(),
        idents: Vec::new(),
    };

    for field in fields {
        let ident = field.ident.as_ref().unwrap();
        let ty = &field.ty;
        let is_vec = is_vec_type(ty);
        let is_option = is_option_type(ty);
        let field_name = resolve_field_name(ident, &field.attrs, rename_all);
        let limit_tokens = extract_limit_tokens(&field.attrs);
        let default_kind = resolve_default_kind(&field.attrs, struct_default);

        let parse_ty = if is_option || is_vec {
            extract_inner_generic(ty).unwrap_or_else(|| ty.clone())
        } else {
            ty.clone()
        };

        push_declaration(&mut cg, ident, ty, is_vec, is_option);
        push_assignment(
            &mut cg,
            ident,
            &parse_ty,
            &field_name,
            &limit_tokens,
            is_vec,
            strict,
        );
        push_post_loop(
            &mut cg,
            ident,
            ty,
            &field_name,
            &default_kind,
            is_option,
            is_vec,
        );
        cg.idents.push(ident);
    }

    cg
}

fn push_declaration<'a>(
    cg: &mut FieldCodegen<'a>,
    ident: &'a syn::Ident,
    ty: &Type,
    is_vec: bool,
    is_option: bool,
) {
    if is_vec {
        cg.declarations
            .push(quote! { let mut #ident: #ty = std::vec::Vec::new(); });
    } else if is_option {
        cg.declarations
            .push(quote! { let mut #ident: #ty = std::option::Option::None; });
    } else {
        cg.declarations
            .push(quote! { let mut #ident: std::option::Option<#ty> = std::option::Option::None; });
    }
}

fn push_assignment<'a>(
    cg: &mut FieldCodegen<'a>,
    ident: &'a syn::Ident,
    parse_ty: &Type,
    field_name: &str,
    limit_tokens: &TokenStream,
    is_vec: bool,
    strict: bool,
) {
    // Explicit turbofish types are required because RPITIT opaque return types
    // prevent the compiler from inferring `TryFromFieldWithState::Self` through `.await`.
    let try_from_call = quote! { <#parse_ty as vespera::multipart::TryFromFieldWithState<__VesperaS__>>::try_from_field_with_state };
    let parse_value = quote! { #try_from_call(__field__, #limit_tokens, __state__).await? };

    let assignment = if is_vec {
        quote! { #ident.push(#parse_value); }
    } else if strict {
        let set_value = quote! { #ident = std::option::Option::Some(#parse_value) };
        let dup_err = quote! { return std::result::Result::Err(vespera::multipart::TypedMultipartError::DuplicateField { field_name: std::string::String::from(#field_name) }) };
        quote! { if #ident.is_none() { #set_value ; } else { #dup_err ; } }
    } else {
        quote! { #ident = std::option::Option::Some(#parse_value); }
    };

    cg.assignments
        .push(quote! { #field_name => { #assignment } });
}

fn push_post_loop<'a>(
    cg: &mut FieldCodegen<'a>,
    ident: &'a syn::Ident,
    ty: &Type,
    field_name: &str,
    default_kind: &DefaultKind,
    is_option: bool,
    is_vec: bool,
) {
    if is_option || is_vec {
        return;
    }

    match default_kind {
        DefaultKind::Trait => {
            cg.post_loop
                .push(quote! { let #ident: #ty = #ident.unwrap_or_default(); });
        }
        DefaultKind::Function(fn_path) => {
            let path: syn::ExprPath =
                syn::parse_str(fn_path).expect("invalid default function path");
            cg.post_loop
                .push(quote! { let #ident: #ty = #ident.unwrap_or_else(#path); });
        }
        DefaultKind::None => {
            cg.post_loop.push(quote! {
                let #ident = #ident.ok_or(
                    vespera::multipart::TypedMultipartError::MissingField {
                        field_name: std::string::String::from(#field_name)
                    }
                )?;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::Fields;

    fn parse_fields_from(code: &str) -> syn::DeriveInput {
        syn::parse_str(code).unwrap()
    }

    fn get_named_fields(
        input: &syn::DeriveInput,
    ) -> &syn::punctuated::Punctuated<syn::Field, syn::token::Comma> {
        match &input.data {
            syn::Data::Struct(s) => match &s.fields {
                Fields::Named(n) => &n.named,
                _ => panic!("expected named fields"),
            },
            _ => panic!("expected struct"),
        }
    }

    #[test]
    fn test_process_fields_required_field_generates_parse_value() {
        let input = parse_fields_from("struct T { pub name: String }");
        let fields = get_named_fields(&input);
        let cg = process_fields(fields.iter(), None, false, false);

        let assignment_code = cg
            .assignments
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(assignment_code.contains("TryFromFieldWithState"));
        assert!(assignment_code.contains("try_from_field_with_state"));
        assert!(assignment_code.contains("\"name\""));

        let post_code = cg
            .post_loop
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(post_code.contains("MissingField"));
    }

    #[test]
    fn test_process_fields_strict_required_field_generates_duplicate_check() {
        let input = parse_fields_from("struct T { pub name: String, pub age: i32 }");
        let fields = get_named_fields(&input);
        let cg = process_fields(fields.iter(), None, true, false);

        let assignment_code = cg
            .assignments
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(assignment_code.contains("is_none"));
        assert!(assignment_code.contains("DuplicateField"));
        assert!(assignment_code.contains("\"name\""));
        assert!(assignment_code.contains("\"age\""));
        assert!(assignment_code.contains("TryFromFieldWithState"));
    }

    #[test]
    fn test_process_fields_vec_field_generates_push() {
        let input = parse_fields_from("struct T { pub tags: Vec<String> }");
        let fields = get_named_fields(&input);
        let cg = process_fields(fields.iter(), None, false, false);

        let decl_code = cg
            .declarations
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(decl_code.contains("Vec :: new"));

        let assignment_code = cg
            .assignments
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(assignment_code.contains("push"));
        assert!(cg.post_loop.is_empty());
    }

    #[test]
    fn test_process_fields_option_field_no_missing_check() {
        let input = parse_fields_from("struct T { pub bio: Option<String> }");
        let fields = get_named_fields(&input);
        let cg = process_fields(fields.iter(), None, false, false);

        let decl_code = cg
            .declarations
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(decl_code.contains("Option :: None"));
        assert!(cg.post_loop.is_empty());
    }

    #[test]
    fn test_process_fields_strict_vec_field_uses_push_not_duplicate() {
        let input = parse_fields_from("struct T { pub tags: Vec<String> }");
        let fields = get_named_fields(&input);
        let cg = process_fields(fields.iter(), None, true, false);

        let assignment_code = cg
            .assignments
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(assignment_code.contains("push"));
        assert!(!assignment_code.contains("DuplicateField"));
    }

    #[test]
    fn test_process_fields_mixed_types() {
        let input = parse_fields_from(
            "struct T { pub name: String, pub tags: Vec<String>, pub bio: Option<String> }",
        );
        let fields = get_named_fields(&input);
        let cg = process_fields(fields.iter(), None, false, false);

        assert_eq!(cg.idents.len(), 3);
        assert_eq!(cg.declarations.len(), 3);
        assert_eq!(cg.assignments.len(), 3);
        assert_eq!(cg.post_loop.len(), 1);
    }
}
