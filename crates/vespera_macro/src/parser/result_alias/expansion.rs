//! In-place expansion of a handler's return type through the alias chain
//! (the lookup rules are in the parent module's docs).

use super::*;

/// Expand the return type of `sig` in place through the aliases found in
/// `same_file` (the handler's file) and (if given) the crate source
/// directory `src_dir`.
///
/// The top-level type is expanded, and so are the positions a response is
/// built from: the elements of a tuple and the arguments of a `Result`
/// (`Result<Created<User>, AppError>` with `type Created<T> = (StatusCode,
/// Json<T>)` documents the `Json<User>` body). The nesting is bounded by
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
    let origin = Origin {
        handler: &handler,
        same_path,
        same_file,
        src_dir,
    };
    expand_position(ty, &origin, 0);
    quote::quote!(#ty).to_string() != before
}

/// Where the return type being expanded was written.
struct Origin<'a> {
    handler: &'a syn::Ident,
    same_path: Option<&'a Path>,
    same_file: Option<&'a syn::File>,
    src_dir: Option<&'a Path>,
}

/// Expands `ty` through its alias chain, then the response positions nested
/// in the result (tuple elements, `Result` arguments), `depth` levels deep.
fn expand_position(ty: &mut Type, origin: &Origin<'_>, depth: usize) {
    if depth >= MAX_ALIAS_DEPTH {
        return;
    }
    expand_alias_chain(ty, origin);
    match ty {
        Type::Tuple(tuple) => {
            for elem in &mut tuple.elems {
                expand_position(elem, origin, depth + 1);
            }
        }
        Type::Path(type_path) => {
            if let Some(segment) = type_path.path.segments.last_mut()
                && segment.ident == "Result"
                && let syn::PathArguments::AngleBracketed(args) = &mut segment.arguments
            {
                for arg in &mut args.args {
                    if let syn::GenericArgument::Type(inner) = arg {
                        expand_position(inner, origin, depth + 1);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Follows the alias chain of `ty` in place (see the module docs).
fn expand_alias_chain(ty: &mut Type, origin: &Origin<'_>) {
    let mut scope = origin.same_file.map(|file| {
        NextScope::Ready(Scope {
            module: fn_module(&file.items, origin.handler).unwrap_or_default(),
            file: Cow::Borrowed(file),
            path: origin.same_path.map(source_key),
        })
    });
    // Keyed by the resolved definition, not the ident: `type Result<T> =
    // crate::error::Result<Json<T>>;` names another module's `Result`.
    let mut seen: Vec<DefinitionKey> = Vec::new();
    while seen.len() < MAX_ALIAS_DEPTH {
        let Some(name) = alias_candidate(ty) else {
            break;
        };
        // An alias found under `src/` names its file, parsed only now that
        // the alias body turned out to be another alias candidate.
        let here = scope.take().and_then(NextScope::materialize);
        if matches!(&*ty, Type::Path(path) if is_external_path(&path.path, here.as_ref())) {
            break;
        }
        let qualifier = alias_qualifier(ty);
        let Some((alias, alias_scope)) =
            find_alias(&name, qualifier.as_ref(), here.as_ref(), origin.src_dir)
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
        scope = Some(alias_scope);
    }
}
