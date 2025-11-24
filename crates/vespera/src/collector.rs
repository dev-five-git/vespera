//! Collector for routes and structs

use crate::file_utils::{collect_files, file_to_segments};
use crate::metadata::{CollectedMetadata, RouteMetadata, StructMetadata};
use crate::route::extract_route_info;
use std::path::Path;
use syn::Item;

/// Collect routes and structs from a folder
pub fn collect_metadata(folder_path: &Path, folder_name: &str) -> CollectedMetadata {
    let mut metadata = CollectedMetadata::new(folder_name.to_string());

    let files = match collect_files(folder_path) {
        Ok(files) => files,
        Err(_) => return metadata,
    };

    for file in files {
        if !file.extension().map(|e| e == "rs").unwrap_or(false) {
            continue;
        }

        let content = match std::fs::read_to_string(&file) {
            Ok(content) => content,
            Err(_) => continue,
        };

        let file_ast = match syn::parse_file(&content) {
            Ok(ast) => ast,
            Err(_) => continue,
        };

        // Get module path
        let segments = if let Ok(file_stem) = file.strip_prefix(folder_path) {
            file_to_segments(file_stem, folder_path)
        } else {
            continue;
        };

        let module_path = if folder_name.is_empty() {
            segments.join("::")
        } else {
            format!("{}::{}", folder_name, segments.join("::"))
        };

        let file_path = file.display().to_string();

        // Collect routes
        for item in &file_ast.items {
            if let Item::Fn(fn_item) = item {
                if let Some(route_info) = extract_route_info(&fn_item.attrs) {
                    let route_path = if let Some(custom_path) = &route_info.path {
                        let base = format!("/{}", segments.join("/"));
                        let trimmed_base = base.trim_end_matches('/');
                        format!("{}/{}", trimmed_base, custom_path.trim_start_matches('/'))
                    } else {
                        format!("/{}", segments.join("/"))
                    };

                    metadata.routes.push(RouteMetadata {
                        method: route_info.method,
                        path: route_path,
                        function_name: fn_item.sig.ident.to_string(),
                        module_path: module_path.clone(),
                        file_path: file_path.clone(),
                        signature: quote::quote!(#fn_item).to_string(),
                    });
                }
            }

            // Collect structs with Schema derive
            if let Item::Struct(struct_item) = item {
                // Check if struct has Schema derive by checking attribute tokens
                let has_schema = struct_item.attrs.iter().any(|attr| {
                    if attr.path().is_ident("derive") {
                        // Convert attribute to tokens and check for Schema
                        let tokens = quote::quote!(#attr).to_string();
                        tokens.contains("Schema")
                    } else {
                        false
                    }
                });

                if has_schema {
                    metadata.structs.push(StructMetadata {
                        name: struct_item.ident.to_string(),
                        module_path: module_path.clone(),
                        file_path: file_path.clone(),
                        definition: quote::quote!(#struct_item).to_string(),
                    });
                }
            }
        }
    }

    metadata
}
