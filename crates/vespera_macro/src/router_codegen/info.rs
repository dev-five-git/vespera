//! `info` object arguments of the `vespera!` macro.
//!
//! `description`, `summary`, `terms_of_service`, `contact_name`,
//! `contact_url`, `contact_email`, `license`, `license_name` and
//! `license_url` are flat string arguments that fill the OpenAPI 3.1
//! [Info Object](https://spec.openapis.org/oas/v3.1.0#info-object).

use std::collections::BTreeMap;

use syn::LitStr;
use vespera_core::openapi::{Contact, Info, License};

/// Every `vespera!` argument handled by this module.
pub const INFO_FIELDS: &[&str] = &[
    "description",
    "summary",
    "terms_of_service",
    "contact_name",
    "contact_url",
    "contact_email",
    "license",
    "license_name",
    "license_url",
];

/// Validated `info` arguments (all optional).
#[derive(Clone, Debug, Default, Hash, PartialEq, Eq)]
pub struct InfoArgs {
    pub description: Option<String>,
    pub summary: Option<String>,
    pub terms_of_service: Option<String>,
    pub contact_name: Option<String>,
    pub contact_url: Option<String>,
    pub contact_email: Option<String>,
    /// SPDX license expression → `license.identifier`.
    pub license: Option<String>,
    pub license_name: Option<String>,
    pub license_url: Option<String>,
}

/// Whether `name` is one of the [`INFO_FIELDS`].
pub fn is_info_field(name: &str) -> bool {
    INFO_FIELDS.contains(&name)
}

fn require_http_url(key: &str, lit: &LitStr) -> syn::Result<()> {
    let value = lit.value();
    if value.starts_with("http://") || value.starts_with("https://") {
        Ok(())
    } else {
        Err(syn::Error::new(
            lit.span(),
            format!(
                "vespera! macro: `{key}` must start with `http://` or `https://`, got \"{value}\""
            ),
        ))
    }
}

fn require_email(lit: &LitStr) -> syn::Result<()> {
    let value = lit.value();
    let valid = value
        .split_once('@')
        .is_some_and(|(local, domain)| !local.is_empty() && !domain.is_empty())
        && !value.chars().any(char::is_whitespace);
    if valid {
        Ok(())
    } else {
        Err(syn::Error::new(
            lit.span(),
            format!("vespera! macro: `contact_email` must be an email address, got \"{value}\""),
        ))
    }
}

/// Validate the raw `info` arguments collected by the `vespera!` parser.
///
/// - `terms_of_service` / `contact_url` / `license_url` must be absolute
///   `http(s)` URLs, `contact_email` must look like an address.
/// - `license` (SPDX → `identifier`) and `license_url` are mutually
///   exclusive, as the OpenAPI 3.1 License Object requires.
/// - `license_url` needs `license_name` (the License Object `name` is
///   required and cannot be derived from a URL).
pub fn validate_info(fields: &BTreeMap<String, LitStr>) -> syn::Result<InfoArgs> {
    for key in ["terms_of_service", "contact_url", "license_url"] {
        if let Some(lit) = fields.get(key) {
            require_http_url(key, lit)?;
        }
    }
    if let Some(lit) = fields.get("contact_email") {
        require_email(lit)?;
    }
    if let (Some(_), Some(url)) = (fields.get("license"), fields.get("license_url")) {
        return Err(syn::Error::new(
            url.span(),
            "vespera! macro: `license` (SPDX identifier) and `license_url` are mutually exclusive in OpenAPI 3.1; use `license_name` + `license_url` for a non-SPDX license",
        ));
    }
    if let Some(url) = fields.get("license_url")
        && !fields.contains_key("license_name")
    {
        return Err(syn::Error::new(
            url.span(),
            "vespera! macro: `license_url` requires `license_name`",
        ));
    }
    let get = |key: &str| fields.get(key).map(LitStr::value);
    Ok(InfoArgs {
        description: get("description"),
        summary: get("summary"),
        terms_of_service: get("terms_of_service"),
        contact_name: get("contact_name"),
        contact_url: get("contact_url"),
        contact_email: get("contact_email"),
        license: get("license"),
        license_name: get("license_name"),
        license_url: get("license_url"),
    })
}

impl InfoArgs {
    /// Copy the configured fields into a generated [`Info`] object.
    pub fn apply(&self, info: &mut Info) {
        info.description.clone_from(&self.description);
        info.summary.clone_from(&self.summary);
        info.terms_of_service.clone_from(&self.terms_of_service);
        info.contact = (self.contact_name.is_some()
            || self.contact_url.is_some()
            || self.contact_email.is_some())
        .then(|| Contact {
            name: self.contact_name.clone(),
            url: self.contact_url.clone(),
            email: self.contact_email.clone(),
        });
        info.license = self
            .license_name
            .as_ref()
            .or(self.license.as_ref())
            .map(|name| License {
                name: name.clone(),
                identifier: self.license.clone(),
                url: self.license_url.clone(),
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proc_macro2::Span;
    use rstest::rstest;

    fn fields(pairs: &[(&str, &str)]) -> BTreeMap<String, LitStr> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), LitStr::new(v, Span::call_site())))
            .collect()
    }

    fn info_json(pairs: &[(&str, &str)]) -> String {
        let mut info = Info::default();
        validate_info(&fields(pairs)).unwrap().apply(&mut info);
        serde_json::to_string_pretty(&info).unwrap()
    }

    #[rstest]
    #[case::empty("info_empty", &[])]
    #[case::spdx_license("info_spdx_license", &[("license", "MIT OR Apache-2.0")])]
    #[case::spdx_license_with_name(
        "info_spdx_license_with_name",
        &[("license", "Apache-2.0"), ("license_name", "Apache License 2.0")]
    )]
    #[case::url_license(
        "info_url_license",
        &[("license_name", "Proprietary"), ("license_url", "https://example.com/license")]
    )]
    #[case::contact_email_only("info_contact_email_only", &[("contact_email", "api@example.com")])]
    #[case::full(
        "info_full",
        &[
            ("description", "Pet store API"),
            ("summary", "Pets"),
            ("terms_of_service", "https://example.com/terms"),
            ("contact_name", "API Team"),
            ("contact_url", "https://example.com/support"),
            ("contact_email", "api@example.com"),
            ("license", "MIT"),
        ]
    )]
    fn apply_builds_info(#[case] name: &str, #[case] pairs: &[(&str, &str)]) {
        insta::assert_snapshot!(name, info_json(pairs));
    }

    #[rstest]
    #[case::tos_not_url(&[("terms_of_service", "/terms")], "`terms_of_service` must start with")]
    #[case::contact_url_not_url(&[("contact_url", "example.com")], "`contact_url` must start with")]
    #[case::license_url_not_url(
        &[("license_name", "X"), ("license_url", "ftp://x")],
        "`license_url` must start with"
    )]
    #[case::email_no_at(&[("contact_email", "nobody")], "`contact_email` must be an email")]
    #[case::email_empty_local(&[("contact_email", "@example.com")], "`contact_email` must be an email")]
    #[case::email_empty_domain(&[("contact_email", "a@")], "`contact_email` must be an email")]
    #[case::email_whitespace(&[("contact_email", "a b@example.com")], "`contact_email` must be an email")]
    #[case::license_and_url(
        &[("license", "MIT"), ("license_name", "MIT"), ("license_url", "https://x.dev")],
        "mutually exclusive"
    )]
    #[case::url_without_name(&[("license_url", "https://x.dev")], "requires `license_name`")]
    fn validate_rejects(#[case] pairs: &[(&str, &str)], #[case] message: &str) {
        let err = validate_info(&fields(pairs)).unwrap_err().to_string();
        assert!(err.contains(message), "{err}");
    }

    #[test]
    fn info_field_names() {
        assert!(INFO_FIELDS.iter().all(|field| is_info_field(field)));
        assert!(!is_info_field("title"));
    }
}
