//! Static configuration: supported languages and the Google Translate URL.

/// Supported languages as (code, display name) pairs, shown in the Preferences
/// language pickers.
pub const LANGUAGES: &[(&str, &str)] = &[
    ("uz", "Uzbek"),
    ("ru", "Russian"),
    ("en", "English"),
    ("tr", "Turkish"),
    ("es", "Spanish"),
    ("ar", "Arabic"),
    ("ko", "Korean"),
    ("de", "German"),
];

/// Build the Google Translate URL for a `from` -> `to` language pair.
pub fn build_translate_url(from: &str, to: &str) -> String {
    format!(
        "https://translate.google.com/?sl={}&tl={}&op=translate",
        from, to
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_contains_language_pair() {
        let url = build_translate_url("uz", "en");
        assert!(url.contains("sl=uz"), "missing source language: {url}");
        assert!(url.contains("tl=en"), "missing target language: {url}");
        assert!(url.starts_with("https://translate.google.com/"));
    }
}
