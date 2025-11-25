use crate::args::RouteArgs;

#[derive(Debug)]
pub struct RouteInfo {
    pub method: String,
    pub path: Option<String>,
}

pub fn check_route_by_meta(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::List(meta_list) => {
            (meta_list.path.segments.len() == 2
                && meta_list.path.segments[0].ident == "vespera"
                && meta_list.path.segments[1].ident == "route")
                || (meta_list.path.segments.len() == 1
                    && meta_list.path.segments[0].ident == "route")
        }
        _ => false,
    }
}

pub fn extract_route_info(attrs: &[syn::Attribute]) -> Option<RouteInfo> {
    for attr in attrs {
        // Check if attribute path is "vespera" or "route"
        if check_route_by_meta(&attr.meta) {
            match &attr.meta {
                syn::Meta::List(meta_list) => {
                    // Try to parse as RouteArgs
                    if let Ok(route_args) = meta_list.parse_args::<RouteArgs>() {
                        let method = route_args
                            .method
                            .map(|ident| ident.to_string())
                            .unwrap_or_else(|| "get".to_string());
                        let path = route_args.path.map(|lit_str| lit_str.value());
                        return Some(RouteInfo { method, path });
                    }
                }
                // Try to parse as Meta::NameValue (e.g., #[route = "patch"])
                syn::Meta::NameValue(meta_nv) => {
                    if let syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(lit_str),
                        ..
                    }) = &meta_nv.value
                    {
                        let method_str = lit_str.value().to_lowercase();
                        match method_str.as_str() {
                            "get" | "post" | "put" | "patch" | "delete" | "head" | "options" => {
                                return Some(RouteInfo {
                                    method: method_str,
                                    path: None,
                                });
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }
    None
}

