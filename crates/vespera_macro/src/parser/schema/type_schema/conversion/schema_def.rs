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

/// Plain data read off a definition string's attributes, memoised per macro
/// invocation: every `$ref` to a hub type (`User` referenced from N routes
/// and M schemas, in both the response and the request-view pass) re-parsed
/// the whole definition to read them.  Both are pure functions of the
/// string, and only `String`s / `bool`s are kept (no AST — see
/// [`parse_schema_def`]); the memo is dropped when the epoch changes so it
/// stays bounded in a long-lived proc-macro server.
#[derive(Default)]
// The outer `Option` is "not computed yet", the inner the fact itself.
#[allow(clippy::option_option)]
struct DefFacts {
    /// `#[schema(name = "..")]` (see `component_name`).
    name: Option<Option<String>>,
    /// `#[schema(ref = ..)]` (see `extract_schema_ref_override`).
    ref_override: Option<Option<(String, bool)>>,
}

thread_local! {
    static DEF_FACTS: std::cell::RefCell<(u64, std::collections::HashMap<String, DefFacts>)> =
        std::cell::RefCell::new((0, std::collections::HashMap::new()));
}

/// The memoised `read` of `def`'s [`DefFacts`] slot chosen by `slot`.
fn def_fact<T: Clone>(
    def: &str,
    slot: fn(&mut DefFacts) -> &mut Option<T>,
    compute: impl FnOnce(&str) -> T,
) -> T {
    let epoch = crate::schema_macro::file_cache::current_epoch();
    let hit = DEF_FACTS.with(|facts| {
        let mut facts = facts.borrow_mut();
        if facts.0 != epoch {
            *facts = (epoch, std::collections::HashMap::new());
        }
        facts.1.get_mut(def).and_then(|entry| slot(entry).clone())
    });
    if let Some(value) = hit {
        return value;
    }
    // Computed with no borrow held, so a re-entrant lookup cannot panic.
    let value = compute(def);
    DEF_FACTS.with(|facts| {
        let mut facts = facts.borrow_mut();
        *slot(facts.1.entry(def.to_owned()).or_default()) = Some(value.clone());
    });
    value
}

/// `#[schema(name = "..")]` of the definition `def`, if any.
pub(super) fn schema_name_of(def: &str) -> Option<String> {
    def_fact(
        def,
        |facts| &mut facts.name,
        |def| {
            syn::parse_str::<syn::DeriveInput>(def)
                .ok()
                .and_then(|input| crate::schema_impl::extract_schema_name_attr(&input.attrs))
        },
    )
}

/// `#[schema(ref = ..)]` override (`(component, nullable)`) of the
/// definition `def`, if any.
pub(super) fn ref_override_of(def: &str) -> Option<(String, bool)> {
    def_fact(
        def,
        |facts| &mut facts.ref_override,
        |def| {
            parse_schema_def(def)
                .and_then(|parsed| super::extract_schema_ref_override(parsed.attrs()))
        },
    )
}

/// The expression each const parameter of `generics` takes from `args`
/// (`Buf<u8, 4>` gives `N` = `4`). The non-lifetime parameters and arguments
/// are paired by position, as rustc pairs them; a bare identifier argument
/// (`Buf<u8, LEN>`, parsed as a type) is the path expression it spells. A
/// parameter with no argument takes its declared default (`const N: usize =
/// 4`), if it has one.
pub(super) fn const_argument_exprs(
    generics: &syn::Generics,
    args: &syn::AngleBracketedGenericArguments,
) -> Vec<(String, syn::Expr)> {
    let mut given = args
        .args
        .iter()
        .filter(|arg| !matches!(arg, syn::GenericArgument::Lifetime(_)));
    generics
        .params
        .iter()
        .filter(|param| !matches!(param, syn::GenericParam::Lifetime(_)))
        .filter_map(|param| {
            let arg = given.next();
            let syn::GenericParam::Const(param) = param else {
                return None;
            };
            let expr = match arg {
                None => param.default.as_ref()?.1.clone(),
                Some(syn::GenericArgument::Const(expr)) => expr.clone(),
                Some(syn::GenericArgument::Type(syn::Type::Path(syn::TypePath {
                    qself: None,
                    path,
                    ..
                }))) => syn::Expr::Path(syn::ExprPath {
                    attrs: Vec::new(),
                    qself: None,
                    path: path.clone(),
                }),
                Some(_) => return None,
            };
            Some((param.ident.to_string(), expr))
        })
        .collect()
}

/// The type arguments of `args` (`Buf<u8, LEN>`: `u8`). An argument paired
/// with a const parameter of `generics` is a value even when it parses as a
/// type (a bare identifier), so it is not one.
pub(super) fn type_argument_types(
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

/// Replaces a path expression naming a const parameter by the expression the
/// instantiation gives it.
struct ConstArgs<'a> {
    consts: &'a [(String, syn::Expr)],
}

impl syn::visit_mut::VisitMut for ConstArgs<'_> {
    fn visit_expr_mut(&mut self, expr: &mut syn::Expr) {
        if let syn::Expr::Path(path) = expr
            && path.qself.is_none()
            && let Some((_, value)) = self
                .consts
                .iter()
                .find(|(name, _)| path.path.is_ident(name.as_str()))
        {
            *expr = value.clone();
        } else {
            syn::visit_mut::visit_expr_mut(self, expr);
        }
    }

    /// A const parameter written as a generic argument (`Buf<T, N>`, parsed as
    /// a type) becomes the const argument it stands for.
    fn visit_generic_argument_mut(&mut self, arg: &mut syn::GenericArgument) {
        if let syn::GenericArgument::Type(syn::Type::Path(syn::TypePath {
            qself: None, path, ..
        })) = arg
            && let Some((_, value)) = self
                .consts
                .iter()
                .find(|(name, _)| path.is_ident(name.as_str()))
        {
            *arg = syn::GenericArgument::Const(value.clone());
        } else {
            syn::visit_mut::visit_generic_argument_mut(self, arg);
        }
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

    /// Replace the const parameters `consts` (see [`const_argument_exprs`]) by
    /// their argument expressions in every field type, so an array length
    /// `[T; N]` becomes the literal the instantiation gives (`[T; 4]`, which
    /// pins `minItems` / `maxItems`).
    pub(super) fn substitute_consts(&mut self, consts: &[(String, syn::Expr)]) {
        let fields: Vec<&mut syn::Field> = match self {
            Self::Struct(item) => item.fields.iter_mut().collect(),
            Self::Enum(item) => item
                .variants
                .iter_mut()
                .flat_map(|variant| variant.fields.iter_mut())
                .collect(),
        };
        let mut visitor = ConstArgs { consts };
        for field in fields {
            syn::visit_mut::VisitMut::visit_type_mut(&mut visitor, &mut field.ty);
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

    /// The memoised facts match a fresh parse, on a miss and on a hit, and
    /// survive no epoch change.
    #[test]
    fn definition_facts_are_memoised_per_epoch() {
        let named = "#[schema(name = \"UserDto\")] struct User { id: i32 }";
        let reffed = "#[schema(ref = \"Other\")] struct Wrapped { id: i32 }";
        crate::schema_macro::file_cache::bump_epoch();
        for _ in 0..2 {
            assert_eq!(schema_name_of(named).as_deref(), Some("UserDto"));
            assert_eq!(schema_name_of(reffed), None);
            assert_eq!(ref_override_of(named), None);
            assert_eq!(ref_override_of(reffed), Some(("Other".to_owned(), false)));
            assert_eq!(ref_override_of("not rust"), None);
        }
        crate::schema_macro::file_cache::bump_epoch();
        assert_eq!(schema_name_of(named).as_deref(), Some("UserDto"));
        DEF_FACTS.with(|facts| assert_eq!(facts.borrow().1.len(), 1));
    }

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

    /// The generic parameters of `struct X{params}` and the arguments of
    /// `X{args}`.
    fn generics_and_args(
        params: &str,
        args: &str,
    ) -> (syn::Generics, syn::AngleBracketedGenericArguments) {
        let item: syn::ItemStruct = syn::parse_str(&format!("struct X{params} {{}}")).unwrap();
        let path: syn::TypePath = syn::parse_str(&format!("X{args}")).unwrap();
        let syn::PathArguments::AngleBracketed(args) =
            path.path.segments.last().unwrap().arguments.clone()
        else {
            panic!("no generic arguments");
        };
        (item.generics, args)
    }

    /// The `(parameter, argument)` pairs of `params` (a generic list) and the
    /// `args` (an argument list) as printed expressions.
    fn const_pairs(params: &str, args: &str) -> Vec<(String, String)> {
        let (generics, args) = generics_and_args(params, args);
        const_argument_exprs(&generics, &args)
            .into_iter()
            .map(|(name, expr)| (name, quote::ToTokens::to_token_stream(&expr).to_string()))
            .collect()
    }

    /// Const parameters pair with arguments by position, lifetimes skipped on
    /// both sides; a type parameter's slot or a non-value argument names
    /// nothing, and a missing argument takes the parameter's default, if any.
    #[rstest]
    #[case::positional("<'a, T, const N: usize>", "<'static, u8, 4>", &[("N", "4")])]
    #[case::no_lifetime_given("<'a, T, const N: usize>", "<u8, 4>", &[("N", "4")])]
    #[case::path_argument("<T, const N: usize>", "<u8, LEN>", &[("N", "LEN")])]
    #[case::block_argument("<T, const N: usize>", "<u8, { 2 * 2 }>", &[("N", "{ 2 * 2 }")])]
    #[case::non_value_argument("<T, const N: usize>", "<u8, [u8; 2]>", &[])]
    #[case::type_parameter_slot("<const N: usize, T>", "<4, u8>", &[("N", "4")])]
    #[case::missing_argument("<T, const N: usize>", "<u8>", &[])]
    #[case::missing_argument_takes_default("<T, const N: usize = 4>", "<u8>", &[("N", "4")])]
    #[case::given_argument_beats_default("<T, const N: usize = 4>", "<u8, 2>", &[("N", "2")])]
    #[case::default_after_type_slots(
        "<T, const N: usize, const M: usize = 3>",
        "<u8, 2>",
        &[("N", "2"), ("M", "3")]
    )]
    fn const_arguments_pair_by_position(
        #[case] params: &str,
        #[case] args: &str,
        #[case] expected: &[(&str, &str)],
    ) {
        let expected: Vec<(String, String)> = expected
            .iter()
            .map(|(name, expr)| ((*name).to_string(), (*expr).to_string()))
            .collect();
        assert_eq!(const_pairs(params, args), expected);
    }

    /// Type arguments are the ones paired with a type parameter (or with no
    /// parameter at all): a bare identifier in a const slot is not a type.
    #[rstest]
    #[case::bare_identifier_in_const_slot("<T, const N: usize>", "<u8, LEN>", &["u8"])]
    #[case::const_slot_before_type("<const N: usize, T>", "<LEN, u8>", &["u8"])]
    #[case::lifetimes_skipped("<'a, T, const N: usize>", "<'static, u8, 4>", &["u8"])]
    #[case::extra_argument_kept("<T>", "<u8, u16>", &["u8", "u16"])]
    fn type_arguments_skip_const_slots(
        #[case] params: &str,
        #[case] args: &str,
        #[case] expected: &[&str],
    ) {
        let (generics, args) = generics_and_args(params, args);
        let printed: Vec<String> = type_argument_types(&generics, &args)
            .iter()
            .map(|ty| quote::ToTokens::to_token_stream(ty).to_string())
            .collect();
        assert_eq!(printed, expected);
    }

    /// A const parameter's uses in field types (array lengths, also inside
    /// other types, of structs and of every enum variant) become its argument;
    /// other names stay.
    #[test]
    fn const_arguments_replace_array_lengths() {
        let mut def = parse_schema_def(
            "struct W<T, const N: usize> { a: [T; N], b: Vec<[u8; N]>, c: [u8; M] }",
        )
        .unwrap();
        let u8_ty: Type = syn::parse_quote!(u8);
        let four: syn::Expr = syn::parse_quote!(4);
        def.substitute(&["T".to_string()], &[&u8_ty]);
        def.substitute_consts(&[("N".to_string(), four)]);
        let SchemaDef::Struct(item) = &def else {
            panic!("a struct");
        };
        let types: Vec<String> = item
            .fields
            .iter()
            .map(|field| quote::ToTokens::to_token_stream(&field.ty).to_string())
            .collect();
        assert_eq!(types, ["[u8 ; 4]", "Vec < [u8 ; 4] >", "[u8 ; M]"]);

        let mut def =
            parse_schema_def("enum E<const N: usize> { A([u8; N]), B { x: [u8; N] } }").unwrap();
        let two: syn::Expr = syn::parse_quote!(2);
        def.substitute_consts(&[("N".to_string(), two)]);
        let SchemaDef::Enum(item) = &def else {
            panic!("an enum");
        };
        let lengths: Vec<String> = item
            .variants
            .iter()
            .flat_map(|variant| variant.fields.iter())
            .map(|field| quote::ToTokens::to_token_stream(&field.ty).to_string())
            .collect();
        assert_eq!(lengths, ["[u8 ; 2]", "[u8 ; 2]"]);
    }
}
