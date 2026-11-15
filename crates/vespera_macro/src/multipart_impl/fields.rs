use std::collections::HashSet;

use proc_macro2::TokenStream;
use quote::quote;
use syn::Type;

use super::attrs::{
    FormDataAttrs, extract_limit_tokens, parse_form_data, resolve_default_kind, resolve_field_name,
};
use super::types::{extract_inner_generic, is_option_type, is_vec_type};
use crate::parser::schema::{extract_aliases, extract_serde_skip_flags};

/// Collected codegen fragments for each struct field.
pub(super) struct FieldCodegen<'a> {
    pub(super) declarations: Vec<TokenStream>,
    pub(super) assignments: Vec<TokenStream>,
    pub(super) post_loop: Vec<TokenStream>,
    pub(super) idents: Vec<&'a syn::Ident>,
    /// The type each parsed (non-skipped) field is read as — the
    /// `TryFromFieldWithState` bound a generic struct's impl needs.
    pub(super) parsed_types: Vec<Type>,
    /// The type of each field filled from `Default::default()` — the
    /// `Default` bound a generic struct's impl needs (serde infers it too).
    pub(super) default_types: Vec<Type>,
    /// Malformed `#[form_data(...)]` attributes.
    pub(super) errors: Vec<syn::Error>,
}

/// How a missing field should be handled.
pub(super) enum DefaultKind {
    /// No default — field is required; emit `MissingField` error.
    None,
    /// Use `Default::default()` — from `#[serde(default)]` or `#[form_data(default)]`.
    Trait,
    /// Call a custom function — from `#[serde(default = "path::to::fn")]`.
    Function(String),
    /// The field of the container's default value — from a struct-level
    /// `#[serde(default)]` / `default = "fn"` (bound once as
    /// `__vespera_default__`, see [`super::process_derive`]).
    Container,
}

/// Process all named fields into codegen fragments.
pub(super) fn process_fields<'a>(
    fields: impl Iterator<Item = &'a syn::Field>,
    rename_all: Option<&str>,
    strict: bool,
    struct_default: bool,
    locals: &HashSet<String>,
) -> FieldCodegen<'a> {
    let mut cg = FieldCodegen {
        declarations: Vec::new(),
        assignments: Vec::new(),
        post_loop: Vec::new(),
        idents: Vec::new(),
        parsed_types: Vec::new(),
        default_types: Vec::new(),
        errors: Vec::new(),
    };
    // Wire name -> the field that claimed it first: two fields resolving to
    // one multipart name would make the second `match` arm unreachable and
    // leave that field permanently missing / defaulted.
    let mut wire_names: std::collections::HashMap<String, &syn::Ident> =
        std::collections::HashMap::new();

    for field in fields {
        let ident = field.ident.as_ref().unwrap();
        let ty = &field.ty;
        let is_vec = is_vec_type(ty);
        let is_option = is_option_type(ty);
        let form = parse_form_data(&field.attrs).unwrap_or_else(|err| {
            cg.errors.push(err);
            FormDataAttrs::default()
        });
        let default_kind = resolve_default_kind(&field.attrs, form.default, struct_default);

        // `#[serde(skip)]` / `skip_deserializing`: serde never reads the
        // field, so neither does the form parser — it starts from its
        // default (`default = "fn"` if given) instead of being required.
        let skip = extract_serde_skip_flags(&field.attrs);
        if skip.skip || skip.skip_deserializing {
            let init = match &default_kind {
                DefaultKind::Function(fn_path) => default_fn_call(ident, fn_path, locals),
                DefaultKind::Container => quote! { __vespera_default__.#ident },
                DefaultKind::Trait | DefaultKind::None => {
                    cg.default_types.push(ty.clone());
                    quote! { ::core::default::Default::default() }
                }
            };
            cg.post_loop.push(quote! { let #ident: #ty = #init; });
            cg.idents.push(ident);
            continue;
        }

        let field_name =
            resolve_field_name(ident, &field.attrs, form.field_name.as_deref(), rename_all);
        // `#[serde(alias = "..")]`: the request schema lists every alias as
        // an accepted part name, so the parser matches each of them too.
        let aliases = extract_aliases(&field.attrs);
        for wire in std::iter::once(&field_name).chain(&aliases) {
            if let Some(first) = wire_names.insert(wire.clone(), ident) {
                cg.errors.push(syn::Error::new(
                    ident.span(),
                    format!(
                        "Multipart: fields `{first}` and `{ident}` both read the multipart field \
                         \"{wire}\" — give one a distinct `#[form_data(field_name = \"...\")]`, \
                         `#[serde(rename = \"...\")]` or `#[serde(alias = \"...\")]`"
                    ),
                ));
            }
        }
        let limit_tokens = extract_limit_tokens(form.limit.as_ref());

        let parse_ty = if is_option || is_vec {
            extract_inner_generic(ty).unwrap_or_else(|| ty.clone())
        } else {
            ty.clone()
        };
        cg.parsed_types.push(parse_ty.clone());

        push_declaration(&mut cg, ident, ty, is_vec, is_option);
        push_assignment(
            &mut cg,
            ident,
            &parse_ty,
            (&field_name, &aliases),
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
            (is_option, is_vec),
            locals,
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
    (field_name, aliases): (&str, &[String]),
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
        .push(quote! { #field_name #(| #aliases)* => { #assignment } });
}

fn push_post_loop<'a>(
    cg: &mut FieldCodegen<'a>,
    ident: &'a syn::Ident,
    ty: &Type,
    field_name: &str,
    default_kind: &DefaultKind,
    (is_option, is_vec): (bool, bool),
    locals: &HashSet<String>,
) {
    if is_option || is_vec {
        // A missing part leaves `None` / `[]` — serde's value for an absent
        // `Option` and the multipart reading of "no parts" — unless the
        // field (or container) names another default.
        let fallback = match default_kind {
            DefaultKind::Function(fn_path) => default_fn_call(ident, fn_path, locals),
            DefaultKind::Container => quote! { __vespera_default__.#ident },
            DefaultKind::Trait | DefaultKind::None => return,
        };
        // One-line `quote!` bodies: llvm coverage never marks the inner lines
        // of a multi-line `quote!` nested in this `if` as executed.
        let binding = if is_vec {
            quote! { let #ident: #ty = if #ident.is_empty() { #fallback } else { #ident }; }
        } else {
            quote! { let #ident: #ty = match #ident { std::option::Option::None => #fallback, __vespera_some__ => __vespera_some__ }; }
        };
        cg.post_loop.push(binding);
        return;
    }

    if matches!(default_kind, DefaultKind::Trait) {
        cg.default_types.push(ty.clone());
    }
    cg.post_loop.push(post_loop_binding(
        ident,
        ty,
        field_name,
        default_kind,
        locals,
    ));
}

/// The post-loop `let` that resolves a missing non-`Option`/`Vec` field.
fn post_loop_binding(
    ident: &syn::Ident,
    ty: &Type,
    field_name: &str,
    default_kind: &DefaultKind,
    locals: &HashSet<String>,
) -> TokenStream {
    match default_kind {
        DefaultKind::Trait => quote! { let #ident: #ty = #ident.unwrap_or_default(); },
        DefaultKind::Function(fn_path) => default_fn_post_loop(ident, ty, fn_path, locals),
        DefaultKind::Container => quote! {
            let #ident: #ty = match #ident {
                std::option::Option::Some(__vespera_value__) => __vespera_value__,
                std::option::Option::None => __vespera_default__.#ident,
            };
        },
        DefaultKind::None => quote! {
            let #ident = #ident.ok_or(
                vespera::multipart::TypedMultipartError::MissingField {
                    field_name: std::string::String::from(#field_name)
                }
            )?;
        },
    }
}

/// Post-loop binding for `default = "path::to::fn"`.
fn default_fn_post_loop(
    ident: &syn::Ident,
    ty: &Type,
    fn_path: &str,
    locals: &HashSet<String>,
) -> TokenStream {
    let default_fn = default_fn_expr(ident, fn_path, locals);
    quote! { let #ident: #ty = #ident.unwrap_or_else(#default_fn); }
}

/// Initialiser calling `default = "path::to::fn"` (for a skipped field).
pub(super) fn default_fn_call(
    ident: &syn::Ident,
    fn_path: &str,
    locals: &HashSet<String>,
) -> TokenStream {
    let default_fn = default_fn_expr(ident, fn_path, locals);
    quote! { (#default_fn)() }
}

/// The `default = "path::to::fn"` function as an expression.
///
/// The derived fn binds one local per field (`locals`), so a one-segment
/// path naming a field (`#[serde(default = "tags")] tags: Vec<String>`)
/// would call the LOCAL; it is spelled `self::tags` (the module item serde
/// means) instead.
fn default_fn_expr(ident: &syn::Ident, fn_path: &str, locals: &HashSet<String>) -> TokenStream {
    // A malformed user-supplied `default = "..."` path must surface as a
    // clean, span-attached compile error at the field — not an internal
    // macro panic (`.expect`) that prints no source location and aborts
    // expansion of the whole derive.
    match syn::parse_str::<syn::ExprPath>(fn_path) {
        Ok(path)
            if path.qself.is_none()
                && path
                    .path
                    .get_ident()
                    .is_some_and(|name| locals.contains(&name.to_string())) =>
        {
            quote! { self::#path }
        }
        Ok(path) => quote! { #path },
        Err(err) => {
            let compile_err = syn::Error::new_spanned(
                ident,
                format!("invalid `default` function path `{fn_path}`: {err}"),
            )
            .to_compile_error();
            // Emit the diagnostic and still yield a function (unreachable
            // fallback) so the malformed default does not cascade into
            // spurious "cannot find value" errors downstream.
            quote! { { #compile_err || ::core::unreachable!() } }
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
        let cg = process_fields(fields.iter(), None, false, false, &HashSet::new());

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
        let cg = process_fields(fields.iter(), None, true, false, &HashSet::new());

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
        let cg = process_fields(fields.iter(), None, false, false, &HashSet::new());

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
        let cg = process_fields(fields.iter(), None, false, false, &HashSet::new());

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
        let cg = process_fields(fields.iter(), None, true, false, &HashSet::new());

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
        let cg = process_fields(fields.iter(), None, false, false, &HashSet::new());

        assert_eq!(cg.idents.len(), 3);
        assert_eq!(cg.declarations.len(), 3);
        assert_eq!(cg.assignments.len(), 3);
        assert_eq!(cg.post_loop.len(), 1);
    }

    /// `#[serde(alias)]` names join the match arm, and an alias colliding with
    /// another field's name is the duplicate-wire-name error.
    #[test]
    fn test_process_fields_serde_alias_arms_and_collisions() {
        let input = parse_fields_from(
            r#"struct T { #[serde(alias = "upload", alias = "doc")] pub file: String }"#,
        );
        let cg = process_fields(
            get_named_fields(&input).iter(),
            None,
            false,
            false,
            &HashSet::new(),
        );
        assert!(cg.errors.is_empty());
        assert!(
            cg.assignments[0]
                .to_string()
                .starts_with("\"file\" | \"upload\" | \"doc\" =>")
        );

        let input = parse_fields_from(
            r#"struct T { pub upload: String, #[serde(alias = "upload")] pub file: String }"#,
        );
        let cg = process_fields(
            get_named_fields(&input).iter(),
            None,
            false,
            false,
            &HashSet::new(),
        );
        assert_eq!(cg.errors.len(), 1);
        assert!(cg.errors[0].to_string().contains("\"upload\""));
    }
}
