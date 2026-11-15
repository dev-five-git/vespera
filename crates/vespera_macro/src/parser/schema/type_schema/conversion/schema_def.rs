//! A known schema's parsed definition, and the generic substitution applied
//! to it before a concrete instantiation (`Page<User>`) is inlined.

use proc_macro2::{TokenStream, TokenTree};
use syn::Type;

use super::super::super::generics::substitute_type;

/// Parse a known schema's definition (a struct or an enum) into a `syn::Item`.
///
/// MUST NOT memoise the parsed AST in a `thread_local` (or any storage that
/// outlives the macro invocation).  A `syn::ItemStruct` parsed during real
/// proc-macro expansion holds bridge-backed `proc_macro2` tokens whose `Drop`
/// calls into the proc-macro bridge; if such a value survived in TLS until the
/// proc-macro server thread exits, that drop would run with NO active expansion
/// and abort the compile ("procedural macro API is used outside of a procedural
/// macro" -> STATUS_STACK_BUFFER_OVERRUN).  Every AST returned here is therefore
/// dropped within the same invocation that parsed it.
pub(super) fn parse_schema_def(def: &str) -> Option<SchemaDef> {
    match syn::parse_str::<syn::Item>(def).ok()? {
        syn::Item::Struct(item) => Some(SchemaDef::Struct(item)),
        syn::Item::Enum(item) => Some(SchemaDef::Enum(item)),
        _ => None,
    }
}

/// A known schema's parsed definition.
pub(super) enum SchemaDef {
    Struct(syn::ItemStruct),
    Enum(syn::ItemEnum),
}

impl SchemaDef {
    pub(super) const fn attrs(&self) -> &Vec<syn::Attribute> {
        match self {
            Self::Struct(item) => &item.attrs,
            Self::Enum(item) => &item.attrs,
        }
    }

    pub(super) const fn generics_mut(&mut self) -> &mut syn::Generics {
        match self {
            Self::Struct(item) => &mut item.generics,
            Self::Enum(item) => &mut item.generics,
        }
    }

    /// Replace the generic parameters `params` by `concrete` in every field
    /// type and in the types the attributes name (`#[schema(value_type =
    /// Vec<T>)]`, `#[serde(into = "Wire<T>")]`) — container, variants and
    /// fields (all variants of an enum).
    pub(super) fn substitute(&mut self, params: &[String], concrete: &[&Type]) {
        let mut attrs: Vec<&mut Vec<syn::Attribute>> = Vec::new();
        let mut fields: Vec<&mut syn::Field> = Vec::new();
        match self {
            Self::Struct(item) => {
                attrs.push(&mut item.attrs);
                fields.extend(item.fields.iter_mut());
            }
            Self::Enum(item) => {
                attrs.push(&mut item.attrs);
                for variant in &mut item.variants {
                    attrs.push(&mut variant.attrs);
                    fields.extend(variant.fields.iter_mut());
                }
            }
        }
        for field in fields {
            field.ty = substitute_type(&field.ty, params, concrete);
            attrs.push(&mut field.attrs);
        }
        for attr in attrs.into_iter().flatten() {
            if let syn::Meta::List(list) = &mut attr.meta
                && (list.path.is_ident("schema") || list.path.is_ident("serde"))
            {
                list.tokens = substitute_tokens(list.tokens.clone(), params, concrete);
            }
        }
    }
}

/// `tokens` of an attribute list with the type after `value_type =` and the
/// type string after `into =` / `from =` / `try_from =` substituted (other
/// keys, `rename = "T"` included, are left alone).
fn substitute_tokens(tokens: TokenStream, params: &[String], concrete: &[&Type]) -> TokenStream {
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    let mut out = TokenStream::new();
    let mut index = 0;
    while index < trees.len() {
        let key = match &trees[index] {
            TokenTree::Ident(ident) => ident.to_string(),
            TokenTree::Group(group) => {
                let mut inner = proc_macro2::Group::new(
                    group.delimiter(),
                    substitute_tokens(group.stream(), params, concrete),
                );
                inner.set_span(group.span());
                out.extend([TokenTree::Group(inner)]);
                index += 1;
                continue;
            }
            _ => String::new(),
        };
        let assigned =
            matches!(trees.get(index + 1), Some(TokenTree::Punct(p)) if p.as_char() == '=');
        out.extend(trees[index..=index].iter().cloned());
        index += 1;
        if !assigned {
            continue;
        }
        out.extend(trees[index..=index].iter().cloned());
        index += 1;
        let end = value_end(&trees, index);
        let value: TokenStream = trees[index..end].iter().cloned().collect();
        out.extend(substitute_value(&key, value, params, concrete));
        index = end;
    }
    out
}

/// The index of the `,` ending the value that starts at `start` (or
/// `trees.len()`): the first comma outside every `<..>` pair, since `<` /
/// `>` are plain puncts (`HashMap<String, T>` is one value). The `>` of a
/// `->` closes nothing.
fn value_end(trees: &[TokenTree], start: usize) -> usize {
    let mut depth = 0usize;
    let mut after_dash = false;
    for (offset, tree) in trees[start..].iter().enumerate() {
        let TokenTree::Punct(punct) = tree else {
            after_dash = false;
            continue;
        };
        match punct.as_char() {
            ',' if depth == 0 => return start + offset,
            '<' => depth += 1,
            '>' if !after_dash => depth = depth.saturating_sub(1),
            _ => {}
        }
        after_dash = punct.as_char() == '-';
    }
    trees.len()
}

/// The substituted `value` of `key` (unchanged for other keys or a value
/// that is not a type / type string).
fn substitute_value(
    key: &str,
    value: TokenStream,
    params: &[String],
    concrete: &[&Type],
) -> TokenStream {
    let substituted = match key {
        "value_type" => syn::parse2::<Type>(value.clone())
            .ok()
            .map(|ty| quote::ToTokens::into_token_stream(substitute_type(&ty, params, concrete))),
        "into" | "from" | "try_from" => syn::parse2::<syn::LitStr>(value.clone())
            .ok()
            .and_then(|lit| Some((lit.parse::<Type>().ok()?, lit.span())))
            .map(|(ty, span)| {
                let ty = substitute_type(&ty, params, concrete);
                let text = quote::ToTokens::into_token_stream(ty).to_string();
                quote::ToTokens::into_token_stream(syn::LitStr::new(&text, span))
            }),
        _ => None,
    };
    substituted.unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    /// Field types and the types attributes name are substituted; other
    /// keys (`rename = "T"`, `bound`) and other attributes are not.
    #[rstest]
    #[case::value_type(
        "struct W<T> { #[schema(value_type = Vec<T>, format = \"x\")] a: Foo<T> }",
        "struct W < T > { # [schema (value_type = Vec < String > , format = \"x\")] a : Foo < String > }"
    )]
    #[case::into_from(
        "#[serde(into = \"Wire<T>\", try_from = \"Wire<T>\", rename = \"T\", rename(serialize = \"T\"))] #[derive(Clone)] struct W<T>(T);",
        "# [serde (into = \"Wire < String >\" , try_from = \"Wire < String >\" , rename = \"T\" , rename (serialize = \"T\"))] # [derive (Clone)] struct W < T > (String) ;"
    )]
    #[case::enum_variants(
        "#[serde(from = \"T\")] enum E<T> { #[serde(into = \"T\")] A(#[schema(value_type = T)] T), B { b: T } }",
        "# [serde (from = \"String\")] enum E < T > { # [serde (into = \"String\")] A (# [schema (value_type = String)] String) , B { b : String } }"
    )]
    // A comma inside `<..>` belongs to the value (`HashMap<String, T>`); a
    // `->`'s `>` closes no bracket.
    #[case::value_type_map(
        "struct W<T> { #[schema(value_type = HashMap<String, T>, format = \"x\")] a: T }",
        "struct W < T > { # [schema (value_type = HashMap < String , String > , format = \"x\")] a : String }"
    )]
    #[case::value_type_result(
        "struct W<T> { #[schema(value_type = Result<T, Vec<T>>)] a: T }",
        "struct W < T > { # [schema (value_type = Result < String , Vec < String > >)] a : String }"
    )]
    #[case::value_type_fn_arrow(
        "struct W<T> { #[schema(value_type = Pair<fn() -> u8, T>, nullable)] a: T }",
        "struct W < T > { # [schema (value_type = Pair < fn () -> u8 , String > , nullable)] a : String }"
    )]
    #[case::unparsable_values(
        "struct W<T> { #[schema(value_type = +, into = 1, flag)] #[serde(from = \"<\")] a: T }",
        "struct W < T > { # [schema (value_type = +, into = 1 , flag)] # [serde (from = \"<\")] a : String }"
    )]
    fn attribute_types_are_substituted(#[case] src: &str, #[case] expected: &str) {
        let mut def = parse_schema_def(src).unwrap();
        let concrete: Type = syn::parse_quote!(String);
        def.substitute(&["T".to_string()], &[&concrete]);
        let tokens = match def {
            SchemaDef::Struct(item) => quote::ToTokens::into_token_stream(item),
            SchemaDef::Enum(item) => quote::ToTokens::into_token_stream(item),
        };
        assert_eq!(tokens.to_string(), expected);
    }
}
