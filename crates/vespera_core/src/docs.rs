//! HTML pages for the interactive API documentation served by `vespera!`
//! (`docs_url` → Swagger UI, `redoc_url` → ReDoc).
//!
//! The UI assets load from unpkg at **exact pinned versions** with
//! Subresource Integrity (`integrity` + `crossorigin="anonymous"`), so a
//! compromised or re-published CDN file is refused by the browser instead of
//! running on the API's origin.  The OpenAPI document is embedded inline and
//! escaped for the `<script>` context by [`escape_json_for_script`], so a
//! `</script>` or `<!--` inside a description or example cannot terminate the
//! script element or inject markup.

/// Pinned `swagger-ui-dist` version loaded by [`swagger_ui_html`].
pub const SWAGGER_UI_VERSION: &str = "5.34.0";

/// Pinned `redoc` version loaded by [`redoc_html`].
pub const REDOC_VERSION: &str = "2.5.4";

const SWAGGER_UI_CSS_URL: &str = "https://unpkg.com/swagger-ui-dist@5.34.0/swagger-ui.css";
const SWAGGER_UI_CSS_SRI: &str =
    "sha384-4c3vsE5f+trhFyHrFrLM1fFGUn531NIaC7dcqEWIJdoRQJ5CkJbSuSCwXLpaS1KF";
const SWAGGER_UI_BUNDLE_URL: &str = "https://unpkg.com/swagger-ui-dist@5.34.0/swagger-ui-bundle.js";
const SWAGGER_UI_BUNDLE_SRI: &str =
    "sha384-9Td7MKiIr2S83E1kZyyVi8KdJw/S8CpAvu0llLyMeg0xq/hDGiZJfVn0fV4llCmK";
const SWAGGER_UI_PRESET_URL: &str =
    "https://unpkg.com/swagger-ui-dist@5.34.0/swagger-ui-standalone-preset.js";
const SWAGGER_UI_PRESET_SRI: &str =
    "sha384-My2aDM4r2Mbm3ybHcubKm9O9U8FEjvF/O5nGvE9YK5dzqOTbWEKa79RPJ1krdMaF";
const REDOC_BUNDLE_URL: &str = "https://unpkg.com/redoc@2.5.4/bundles/redoc.standalone.js";
const REDOC_BUNDLE_SRI: &str =
    "sha384-w447zOpYfw/1Tv/5AK9NfHTlQIqE3RVR6KY62jCyy9zNDgO64cMwGGP1Fj0zJVf5";

/// Escape a JSON document for embedding inside an HTML `<script>` element.
///
/// `<`, `>`, `&`, U+2028 and U+2029 are replaced by their `\uXXXX` JSON
/// escapes.  Those characters can only occur inside JSON *strings*, where
/// the escape decodes to the identical character, so the result is still
/// valid JSON (and a valid JS literal) with the same value — but it can no
/// longer contain `</script>`, `<!--` or a JS line terminator.
pub fn escape_json_for_script(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for ch in json.chars() {
        match ch {
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            _ => out.push(ch),
        }
    }
    out
}

/// Render the Swagger UI page with `spec_json` (a serialized OpenAPI
/// document) embedded inline.
pub fn swagger_ui_html(spec_json: &str) -> String {
    let spec = escape_json_for_script(spec_json);
    format!(
        r##"<!DOCTYPE html><html lang="en"><head><meta charset="UTF-8"><title>Swagger UI</title><meta name="viewport" content="width=device-width, initial-scale=1"><link rel="stylesheet" href="{SWAGGER_UI_CSS_URL}" integrity="{SWAGGER_UI_CSS_SRI}" crossorigin="anonymous" /></head><body style="margin: 0; padding: 0;"><div id="swagger-ui"></div><script src="{SWAGGER_UI_BUNDLE_URL}" integrity="{SWAGGER_UI_BUNDLE_SRI}" crossorigin="anonymous"></script><script src="{SWAGGER_UI_PRESET_URL}" integrity="{SWAGGER_UI_PRESET_SRI}" crossorigin="anonymous"></script><script>const openapiSpec = {spec};window.onload = () => {{ SwaggerUIBundle({{ spec: openapiSpec, dom_id: "#swagger-ui", presets: [SwaggerUIBundle.presets.apis, SwaggerUIStandalonePreset], layout: "StandaloneLayout" }}); }};</script></body></html>"##
    )
}

/// Render the ReDoc page with `spec_json` (a serialized OpenAPI document)
/// embedded inline.
pub fn redoc_html(spec_json: &str) -> String {
    let spec = escape_json_for_script(spec_json);
    format!(
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="UTF-8"><title>ReDoc</title><meta name="viewport" content="width=device-width, initial-scale=1"><style>body {{ margin: 0; padding: 0; }}</style></head><body><div id="redoc-container"></div><script src="{REDOC_BUNDLE_URL}" integrity="{REDOC_BUNDLE_SRI}" crossorigin="anonymous"></script><script>const openapiSpec = {spec};Redoc.init(openapiSpec, {{}}, document.getElementById("redoc-container"));</script></body></html>"#
    )
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    /// A spec whose strings try to break out of the inline `<script>`.
    const HOSTILE_SPEC: &str = "{\"info\":{\"description\":\"</script><script>alert(1)</script><!-- a&b \u{2028}\u{2029}\"}}";

    #[rstest]
    #[case::plain(r#"{"a":1}"#, r#"{"a":1}"#)]
    #[case::script_close("</script>", "\\u003c/script\\u003e")]
    #[case::html_comment("<!--", "\\u003c!--")]
    #[case::ampersand("a&b", "a\\u0026b")]
    #[case::line_separators("\u{2028}\u{2029}", "\\u2028\\u2029")]
    #[case::non_ascii_kept("héllo ✓", "héllo ✓")]
    fn escape_json_for_script_cases(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(escape_json_for_script(input), expected);
    }

    #[test]
    fn escaped_json_round_trips_to_the_same_value() {
        let escaped = escape_json_for_script(HOSTILE_SPEC);
        let original: serde_json::Value = serde_json::from_str(HOSTILE_SPEC).unwrap();
        let decoded: serde_json::Value = serde_json::from_str(&escaped).unwrap();
        assert_eq!(original, decoded);
    }

    #[rstest]
    #[case::swagger_ui(swagger_ui_html(HOSTILE_SPEC), 3)]
    #[case::redoc(redoc_html(HOSTILE_SPEC), 2)]
    fn hostile_spec_cannot_close_the_script_element(
        #[case] html: String,
        #[case] script_elements: usize,
    ) {
        // Only the template's own `<script>` elements may close.
        assert_eq!(html.matches("</script>").count(), script_elements);
        assert_eq!(html.matches("<script").count(), script_elements);
        assert!(!html.contains("<!--"));
        assert!(!html.contains('\u{2028}'));
        assert!(!html.contains('\u{2029}'));
    }

    #[rstest]
    #[case::swagger_ui(swagger_ui_html(""), 3)]
    #[case::redoc(redoc_html(""), 1)]
    fn every_asset_is_pinned_with_sri(#[case] html: String, #[case] assets: usize) {
        assert_eq!(html.matches("integrity=\"sha384-").count(), assets);
        assert_eq!(html.matches("crossorigin=\"anonymous\"").count(), assets);
        assert_eq!(html.matches("https://unpkg.com/").count(), assets);
        let pinned = html
            .matches(&format!("swagger-ui-dist@{SWAGGER_UI_VERSION}/"))
            .count()
            + html.matches(&format!("redoc@{REDOC_VERSION}/")).count();
        assert_eq!(pinned, assets);
    }

    #[test]
    fn swagger_ui_html_snapshot() {
        insta::assert_snapshot!(
            "swagger_ui_html_hostile_spec",
            swagger_ui_html(HOSTILE_SPEC)
        );
    }

    #[test]
    fn redoc_html_snapshot() {
        insta::assert_snapshot!("redoc_html_hostile_spec", redoc_html(HOSTILE_SPEC));
    }
}
