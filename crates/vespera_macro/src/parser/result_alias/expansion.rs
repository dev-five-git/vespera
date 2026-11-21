//! In-place expansion of a handler's return type through the alias chain
//! (the lookup rules are in the parent module's docs).

use super::{
    Cow, DefinitionKey, MAX_ALIAS_DEPTH, NextScope, Path, ReturnType, Scope, Type, alias_candidate,
    alias_qualifier, definition_key, find_alias, fn_module, is_external_path, source_key,
};
use crate::{
    parser::schema::{is_builtin_openapi_type, substitute_type},
    schema_macro::type_utils::last_path_segment,
};

/// The prefix of the placeholder idents an alias body's parameters stand for
/// while the body is expanded (see [`expand_alias`]).
const PARAM_PREFIX: &str = "__vespera_alias_param_";

/// Whether an alias can apply to `ty` or to a response position nested in it
/// (a tuple element, a `Result` argument or a `Json` body, as
/// [`expand_position`] walks them).
pub(super) fn has_alias_position(ty: &Type) -> bool {
    if alias_candidate(ty).is_some() {
        return true;
    }
    match ty {
        Type::Tuple(tuple) => tuple.elems.iter().any(has_alias_position),
        Type::Path(type_path) => type_path
            .path
            .segments
            .last()
            .is_some_and(nested_args_have_alias),
        _ => false,
    }
}

/// Whether one of the response positions of `segment` (see
/// [`response_positions`]) has an alias position.
fn nested_args_have_alias(segment: &syn::PathSegment) -> bool {
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return false;
    };
    args.args
        .iter()
        .filter_map(|arg| {
            if let syn::GenericArgument::Type(inner) = arg {
                Some(inner)
            } else {
                None
            }
        })
        .take(response_positions(segment))
        .any(has_alias_position)
}

/// How many leading type arguments of `segment` are response positions an
/// alias can name: every argument of a `Result`, the body of a `Json` (its
/// first argument, the body axum documents), none for any other path. Other
/// wrappers (`Html`, `Sse`, `Bytes`, `Response`, ...) carry no schema of their
/// own, so an alias inside them documents nothing.
fn response_positions(segment: &syn::PathSegment) -> usize {
    if segment.ident == "Json" {
        1
    } else if segment.ident == "Result" {
        usize::MAX
    } else {
        0
    }
}

/// Expand the return type of `sig` in place through the aliases found in
/// `same_file` (the handler's file) and (if given) the crate source
/// directory `src_dir`.
///
/// The top-level type is expanded, and so are the positions a response is
/// built from: the elements of a tuple, the arguments of a `Result`
/// (`Result<Created<User>, AppError>` with `type Created<T> = (StatusCode,
/// Json<T>)` documents the `Json<User>` body) and the body of a `Json`
/// (`Json<UserList>` with `type UserList = Vec<User>` documents the array).
/// Request bodies are not expanded: this function only sees the return type.
/// Each nested position is looked up in the scope its type is written in, so
/// the names of an alias defined in another file resolve there. The nesting is bounded by
/// [`MAX_ALIAS_DEPTH`], and an alias that is already being expanded is not
/// applied again, so a cyclic alias cannot recurse forever.
///
/// Returns `true` when the return type changed.
pub fn expand_return_type_alias(
    sig: &mut syn::Signature,
    same_path: Option<&Path>,
    same_file: Option<&syn::File>,
    src_dir: Option<&Path>,
) -> bool {
    let handler = sig.ident.clone();
    let ReturnType::Type(_, ty) = &mut sig.output else {
        return false;
    };
    let before = quote::quote!(#ty).to_string();
    let handler_scope = same_file.map(|file| {
        NextScope::Ready(Scope {
            module: fn_module(&file.items, &handler).unwrap_or_default(),
            file: Cow::Borrowed(file),
            path: same_path.map(source_key),
        })
    });
    expand_position(ty, handler_scope.as_ref(), src_dir, 0, &[]);
    quote::quote!(#ty).to_string() != before
}

/// Expands `ty`, written in `scope` (`None`: no file known): through the
/// alias it names when one applies ([`expand_alias`]), else the response
/// positions nested in it (tuple elements, `Result` arguments). `depth`
/// levels deep at most. `active` are the alias definitions being expanded
/// around `ty`.
fn expand_position(
    ty: &mut Type,
    scope: Option<&NextScope<'_>>,
    src_dir: Option<&Path>,
    depth: usize,
    active: &[DefinitionKey],
) {
    if depth >= MAX_ALIAS_DEPTH {
        return;
    }
    if let Some(expanded) = expand_alias(ty, scope, src_dir, depth, active) {
        *ty = expanded;
        return;
    }
    match ty {
        Type::Tuple(tuple) => {
            for elem in &mut tuple.elems {
                expand_position(elem, scope, src_dir, depth + 1, active);
            }
        }
        Type::Path(type_path) => {
            if let Some(segment) = type_path.path.segments.last_mut()
                && let limit = response_positions(segment)
                && limit > 0
                && let syn::PathArguments::AngleBracketed(args) = &mut segment.arguments
            {
                let types = args.args.iter_mut().filter_map(|arg| {
                    if let syn::GenericArgument::Type(inner) = arg {
                        Some(inner)
                    } else {
                        None
                    }
                });
                for inner in types.take(limit) {
                    expand_position(inner, scope, src_dir, depth + 1, active);
                }
            }
        }
        _ => {}
    }
}

/// `ty` (written in `scope`) with the alias it names applied and fully
/// expanded, or `None` when no alias applies to it.
///
/// Every name is resolved where it is written, so no part is looked up in the
/// wrong module:
///
/// - the use-site arguments (`Created<User>` in `Api<Created<User>>`) are
///   expanded in `scope`, before they are substituted;
/// - the alias body is expanded in the scope the alias is defined in, while
///   its own parameters are still placeholders (`__vespera_alias_param_N`),
///   which no lookup matches;
/// - only then are the placeholders replaced by the expanded arguments. The
///   result is not expanded again, so an argument's names never meet the
///   body's scope.
///
/// An alias already in `active` (its body is being expanded around `ty`) is
/// not applied again: a cycle stops where it closes.
fn expand_alias(
    ty: &Type,
    scope: Option<&NextScope<'_>>,
    src_dir: Option<&Path>,
    depth: usize,
    active: &[DefinitionKey],
) -> Option<Type> {
    let name = alias_candidate(ty)?;
    if name.starts_with(PARAM_PREFIX) || depth + 1 >= MAX_ALIAS_DEPTH {
        return None;
    }
    // An alias found under `src/` names its file, parsed only now that the
    // type turned out to be an alias candidate.
    let here = scope.cloned().and_then(NextScope::materialize);
    if matches!(ty, Type::Path(path) if is_external_path(&path.path, here.as_ref(), src_dir)) {
        return None;
    }
    let qualifier = alias_qualifier(ty);
    // A builtin OpenAPI name is looked up in the handler's own file only (the
    // crate-wide scan would run for every `String` / `u8` position), except
    // `Result<T>`, the error alias a crate usually declares once in its src.
    let crate_src = src_dir.filter(|_| name == "Result" || !is_builtin_openapi_type(&name));
    let (alias, alias_scope) = find_alias(&name, qualifier.as_ref(), here.as_ref(), crate_src)?;
    let key = definition_key(&alias, &alias_scope);
    if active.contains(&key) {
        return None;
    }
    let segment = last_path_segment(ty)?;
    let mut args: Vec<Type> = match &segment.arguments {
        syn::PathArguments::AngleBracketed(args) => type_arguments(&alias.generics, args),
        // `None` (and the `Fn(..)` sugar, which never reaches a return type).
        _ => Vec::new(),
    };
    let params: Vec<&syn::TypeParam> = alias.generics.type_params().collect();
    if args.len() > params.len() {
        return None;
    }
    let param_names: Vec<String> = params.iter().map(|param| param.ident.to_string()).collect();
    let names: Vec<String> = (0..params.len())
        .map(|index| format!("{PARAM_PREFIX}{index}"))
        .collect();
    let placeholders: Vec<Type> = names
        .iter()
        .map(|name| syn::parse_str(name).ok())
        .collect::<Option<Vec<Type>>>()?;
    let placeholder_refs: Vec<&Type> = placeholders.iter().collect();
    // The use-site arguments are not inside this alias's body: they expand
    // under the caller's active set.
    for arg in &mut args {
        expand_position(arg, scope, src_dir, depth + 1, active);
    }
    let mut body_active = active.to_vec();
    body_active.push(key);
    let mut values: Vec<Type> = Vec::with_capacity(params.len());
    for (index, param) in params.iter().enumerate() {
        let value = if let Some(arg) = args.get(index) {
            arg.clone()
        } else {
            // A default may name earlier parameters (`type R<T, E =
            // Wrapper<T>>`): it is written in the alias's scope, so it is
            // expanded there while those are still placeholders.
            let default = &param.default.as_ref()?.1;
            let mut default =
                substitute_type(default, &param_names[..index], &placeholder_refs[..index]);
            expand_position(
                &mut default,
                Some(&alias_scope),
                src_dir,
                depth + 1,
                &body_active,
            );
            let earlier: Vec<&Type> = values.iter().collect();
            substitute_type(&default, &names[..index], &earlier)
        };
        values.push(value);
    }
    let mut body = substitute_type(&alias.ty, &param_names, &placeholder_refs);
    expand_position(
        &mut body,
        Some(&alias_scope),
        src_dir,
        depth + 1,
        &body_active,
    );
    let concrete: Vec<&Type> = values.iter().collect();
    Some(substitute_type(&body, &names, &concrete))
}

/// The type arguments of a use site that fill the type parameters of an
/// alias with `generics` (`Page<LIMIT, User>` for `type Page<const MAX:
/// usize, T>`: `[User]`). Lifetimes are skipped on both sides and the rest
/// are paired by position, as rustc pairs them. An argument paired with a
/// const parameter is a value even when it parses as a type (a bare
/// identifier such as `LIMIT`), so it is not a type argument. This mirrors
/// `type_argument_types` in `schema/type_schema/conversion/schema_def.rs`.
fn type_arguments(
    generics: &syn::Generics,
    args: &syn::AngleBracketedGenericArguments,
) -> Vec<Type> {
    let mut params = generics
        .params
        .iter()
        .filter(|param| !matches!(param, syn::GenericParam::Lifetime(_)));
    args.args
        .iter()
        .filter(|arg| !matches!(arg, syn::GenericArgument::Lifetime(_)))
        .filter_map(|arg| {
            let param = params.next();
            let syn::GenericArgument::Type(ty) = arg else {
                return None;
            };
            (!matches!(param, Some(syn::GenericParam::Const(_)))).then(|| ty.clone())
        })
        .collect()
}
