//! In-place expansion of a handler's return type through the alias chain
//! (the lookup rules are in the parent module's docs).

use super::{
    Cow, DefinitionKey, MAX_ALIAS_DEPTH, NextScope, Path, ReturnType, Scope, Type, alias_candidate,
    alias_qualifier, apply_alias, definition_key, find_alias, fn_module, is_external_path,
    source_key,
};

/// Expand the return type of `sig` in place through the aliases found in
/// `same_file` (the handler's file) and (if given) the crate source
/// directory `src_dir`.
///
/// The top-level type is expanded, and so are the positions a response is
/// built from: the elements of a tuple and the arguments of a `Result`
/// (`Result<Created<User>, AppError>` with `type Created<T> = (StatusCode,
/// Json<T>)` documents the `Json<User>` body). Each nested position is looked
/// up in the scope of the alias its enclosing type came from, so the names of
/// an alias defined in another file resolve there. The nesting is bounded by
/// [`MAX_ALIAS_DEPTH`], so a cyclic alias cannot recurse forever.
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
    expand_position(ty, handler_scope.as_ref(), src_dir, 0);
    quote::quote!(#ty).to_string() != before
}

/// Expands `ty`, written in `scope` (`None`: no file known), through its alias
/// chain, then the response positions nested in the result (tuple elements,
/// `Result` arguments), `depth` levels deep.
fn expand_position(
    ty: &mut Type,
    scope: Option<&NextScope<'_>>,
    src_dir: Option<&Path>,
    depth: usize,
) {
    if depth >= MAX_ALIAS_DEPTH {
        return;
    }
    // The scope the chain ends in: where the last alias applied is defined,
    // which is where its body's nested names are written.
    let mut scope = scope.cloned();
    expand_alias_chain(ty, &mut scope, src_dir);
    let scope = scope.as_ref();
    match ty {
        Type::Tuple(tuple) => {
            for elem in &mut tuple.elems {
                expand_position(elem, scope, src_dir, depth + 1);
            }
        }
        Type::Path(type_path) => {
            if let Some(segment) = type_path.path.segments.last_mut()
                && segment.ident == "Result"
                && let syn::PathArguments::AngleBracketed(args) = &mut segment.arguments
            {
                for arg in &mut args.args {
                    if let syn::GenericArgument::Type(inner) = arg {
                        expand_position(inner, scope, src_dir, depth + 1);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Follows the alias chain of `ty` in place (see the module docs). `scope` is
/// the scope `ty` is written in; each alias applied replaces it with the scope
/// that alias is defined in, where its body is written.
fn expand_alias_chain(ty: &mut Type, scope: &mut Option<NextScope<'_>>, src_dir: Option<&Path>) {
    // Keyed by the resolved definition, not the ident: `type Result<T> =
    // crate::error::Result<Json<T>>;` names another module's `Result`.
    let mut seen: Vec<DefinitionKey> = Vec::new();
    while seen.len() < MAX_ALIAS_DEPTH {
        let Some(name) = alias_candidate(ty) else {
            break;
        };
        // An alias found under `src/` names its file, parsed only now that
        // the alias body turned out to be another alias candidate.
        let here = scope.clone().and_then(NextScope::materialize);
        if matches!(&*ty, Type::Path(path) if is_external_path(&path.path, here.as_ref())) {
            break;
        }
        let qualifier = alias_qualifier(ty);
        let Some((alias, alias_scope)) =
            find_alias(&name, qualifier.as_ref(), here.as_ref(), src_dir)
        else {
            break;
        };
        let key = definition_key(&alias, &alias_scope);
        if seen.contains(&key) {
            break;
        }
        let Some(next) = apply_alias(&alias, ty) else {
            break;
        };
        *ty = next;
        seen.push(key);
        // The alias body names types where the alias is defined.
        *scope = Some(alias_scope);
    }
}
