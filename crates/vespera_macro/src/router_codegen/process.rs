//! Normalisation of [`AutoRouterInput`] into a builder-friendly form.
//!
//! [`ProcessedVesperaInput`] is the value [`crate::vespera_impl`] consumes when
//! orchestrating the `vespera!` macro — defaults are filled in here so the
//! orchestrator can stay agnostic about parse details.

use vespera_core::openapi::Server;

use super::input::AutoRouterInput;

/// Processed vespera input with extracted values
pub struct ProcessedVesperaInput {
    pub folder_name: String,
    pub openapi_file_names: Vec<String>,
    pub title: Option<String>,
    pub version: Option<String>,
    pub docs_url: Option<String>,
    pub redoc_url: Option<String>,
    pub servers: Option<Vec<Server>>,
    /// Apps to merge (`syn::Path` for code generation)
    pub merge: Vec<syn::Path>,
}

/// Process `AutoRouterInput` into extracted values
pub fn process_vespera_input(input: AutoRouterInput) -> ProcessedVesperaInput {
    ProcessedVesperaInput {
        folder_name: input
            .dir
            .map_or_else(|| "routes".to_string(), |f| f.value()),
        openapi_file_names: input
            .openapi
            .unwrap_or_default()
            .into_iter()
            .map(|f| f.value())
            .collect(),
        title: input.title.map(|t| t.value()),
        version: input.version.map(|v| v.value()),
        docs_url: input.docs_url.map(|u| u.value()),
        redoc_url: input.redoc_url.map(|u| u.value()),
        servers: input.servers.map(|svrs| {
            svrs.into_iter()
                .map(|s| Server {
                    url: s.url,
                    description: s.description,
                    variables: None,
                })
                .collect()
        }),
        merge: input.merge.unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_vespera_input_defaults() {
        let tokens = quote::quote!();
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        let processed = process_vespera_input(input);
        assert_eq!(processed.folder_name, "routes");
        assert!(processed.openapi_file_names.is_empty());
        assert!(processed.title.is_none());
        assert!(processed.docs_url.is_none());
    }

    #[test]
    fn test_process_vespera_input_all_fields() {
        let tokens = quote::quote!(
            dir = "api",
            openapi = ["openapi.json", "api.json"],
            title = "My API",
            version = "1.0.0",
            docs_url = "/docs",
            redoc_url = "/redoc",
            servers = "http://localhost:3000"
        );
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        let processed = process_vespera_input(input);
        assert_eq!(processed.folder_name, "api");
        assert_eq!(
            processed.openapi_file_names,
            vec!["openapi.json", "api.json"]
        );
        assert_eq!(processed.title, Some("My API".to_string()));
        assert_eq!(processed.version, Some("1.0.0".to_string()));
        assert_eq!(processed.docs_url, Some("/docs".to_string()));
        assert_eq!(processed.redoc_url, Some("/redoc".to_string()));
        let servers = processed.servers.unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].url, "http://localhost:3000");
    }

    #[test]
    fn test_process_vespera_input_servers_with_description() {
        let tokens = quote::quote!(
            servers = [{ url = "https://api.example.com", description = "Production" }]
        );
        let input: AutoRouterInput = syn::parse2(tokens).unwrap();
        let processed = process_vespera_input(input);
        let servers = processed.servers.unwrap();
        assert_eq!(servers[0].url, "https://api.example.com");
        assert_eq!(servers[0].description, Some("Production".to_string()));
    }
}
