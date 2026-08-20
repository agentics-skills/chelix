//! DuckDuckGo HTML result parsing and agent-facing Markdown formatting.

use regex::Regex;

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchResultItem {
    pub(crate) title: String,
    pub(crate) url: String,
    pub(crate) snippet: String,
    pub(crate) display_url: String,
}

pub(crate) struct SearchParser {
    anchor: Regex,
    snippet: Regex,
    block_marker: Regex,
    display_url: Regex,
    tags: Regex,
}

impl SearchParser {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            anchor: Regex::new(
                r#"(?i)<a[^>]*class="[^"]*result__a[^"]*"[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#,
            )?,
            snippet: Regex::new(r#"(?i)<a[^>]*class="[^"]*result__snippet[^"]*"[^>]*>(.*?)</a>"#)?,
            block_marker: Regex::new(
                r#"(?i)(?:class|id|name)\s*=\s*["'][^"']*(?:anomaly-modal|captcha|blocked)[^"']*["']"#,
            )?,
            display_url: Regex::new(
                r#"(?i)<(?:span|div)[^>]*class="[^"]*result__url[^"]*"[^>]*>(.*?)</[^>]+>"#,
            )?,
            tags: Regex::new(r"<[^>]+>")?,
        })
    }

    pub(crate) fn is_block_page(&self, html: &str) -> bool {
        html.len() < 1_000 || self.block_marker.is_match(html)
    }

    pub(crate) fn parse(&self, html: &str, num_results: usize) -> Result<Vec<SearchResultItem>> {
        let snippets = self
            .captures(&self.snippet, html, "DuckDuckGo snippet capture")?
            .into_iter()
            .map(|snippet| self.clean_text(snippet))
            .collect::<Vec<_>>();
        let display_urls = self
            .captures(&self.display_url, html, "DuckDuckGo display URL capture")?
            .into_iter()
            .map(|display_url| self.clean_text(display_url))
            .collect::<Vec<_>>();
        let mut results = Vec::new();

        for captures in self.anchor.captures_iter(html) {
            let raw_link = captures
                .get(1)
                .ok_or_else(|| Error::message("DuckDuckGo result URL capture is missing"))?
                .as_str();
            let title_html = captures
                .get(2)
                .ok_or_else(|| Error::message("DuckDuckGo result title capture is missing"))?
                .as_str();
            let title = self.clean_text(title_html);
            if title.is_empty() || raw_link.is_empty() {
                continue;
            }
            let index = results.len();
            results.push(SearchResultItem {
                title,
                url: extract_direct_url(raw_link),
                snippet: snippets.get(index).cloned().unwrap_or_default(),
                display_url: display_urls.get(index).cloned().unwrap_or_default(),
            });
            if results.len() >= num_results {
                break;
            }
        }
        Ok(results)
    }

    fn captures<'a>(
        &self,
        regex: &Regex,
        html: &'a str,
        missing_message: &'static str,
    ) -> Result<Vec<&'a str>> {
        regex
            .captures_iter(html)
            .map(|captures| {
                captures
                    .get(1)
                    .map(|value| value.as_str())
                    .ok_or_else(|| Error::message(missing_message))
            })
            .collect()
    }

    fn clean_text(&self, html: &str) -> String {
        let without_tags = self.tags.replace_all(html, " ");
        decode_entities_once(&without_tags)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn decode_entities_once(input: &str) -> String {
    const ENTITIES: [(&str, &str); 5] = [
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
    ];

    let mut decoded = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(index) = rest.find('&') {
        decoded.push_str(&rest[..index]);
        let candidate = &rest[index..];
        if let Some((entity, replacement)) = ENTITIES
            .iter()
            .find(|(entity, _)| candidate.starts_with(entity))
        {
            decoded.push_str(replacement);
            rest = &candidate[entity.len()..];
        } else {
            decoded.push('&');
            rest = &candidate['&'.len_utf8()..];
        }
    }
    decoded.push_str(rest);
    decoded
}

fn extract_direct_url(raw: &str) -> String {
    let normalized = if raw.starts_with("//") {
        format!("https:{raw}")
    } else if raw.starts_with('/') {
        format!("https://duckduckgo.com{raw}")
    } else {
        raw.to_string()
    };
    let Ok(url) = url::Url::parse(&normalized) else {
        return normalized;
    };
    if url.host_str() == Some("duckduckgo.com")
        && url.path() == "/l/"
        && let Some(uddg) = query_parameter(&url, "uddg")
    {
        return decode_or_original(&uddg);
    }
    if url.host_str() == Some("duckduckgo.com")
        && url.path() == "/y.js"
        && let Some(u3) = query_parameter(&url, "u3")
    {
        let decoded = decode_or_original(&u3);
        if let Ok(nested) = url::Url::parse(&decoded) {
            if let Some(click) = query_parameter(&nested, "ld") {
                return decode_or_original(&click);
            }
            return decoded;
        }
    }
    normalized
}

fn query_parameter(url: &url::Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

fn decode_or_original(value: &str) -> String {
    urlencoding::decode(value)
        .map(std::borrow::Cow::into_owned)
        .unwrap_or_else(|_| value.to_string())
}

pub(crate) fn format_results(query: &str, page: u32, items: &[SearchResultItem]) -> String {
    if items.is_empty() {
        return "No results found.".to_string();
    }
    let mut lines = vec![
        format!("Search results for \"{query}\":"),
        String::new(),
        format!("Page {page}; showing {} result(s)", items.len()),
        String::new(),
    ];
    for (index, item) in items.iter().enumerate() {
        lines.push(format!("{}. [{}]({})", index + 1, item.title, item.url));
        if !item.snippet.is_empty() {
            lines.push(format!("   {}", item.snippet));
        }
        if !item.display_url.is_empty() {
            lines.push(format!("   Source: {}", item.display_url));
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reference_html_shape_and_limits_results() {
        let parser =
            SearchParser::new().unwrap_or_else(|error| panic!("parser creation failed: {error}"));
        let html = r#"
            <a class="result__snippet">First &amp; <b>useful</b> snippet</a>
            <a class="result__snippet">Second snippet</a>
            <span class="result__url">example.com/one</span>
            <div class="result__url">example.com/two</div>
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%253A%252F%252Fexample.com%252Fone">First &amp; Result</a>
            <a class="result__a" href="https://example.com/two">Second Result</a>
        "#;

        let results = parser
            .parse(html, 1)
            .unwrap_or_else(|error| panic!("HTML parsing failed: {error}"));

        assert_eq!(results, vec![SearchResultItem {
            title: "First & Result".to_string(),
            url: "https://example.com/one".to_string(),
            snippet: "First & useful snippet".to_string(),
            display_url: "example.com/one".to_string(),
        }]);
    }

    #[test]
    fn decodes_entities_only_once() {
        assert_eq!(decode_entities_once("&amp;lt; &lt;"), "&lt; <");
    }

    #[test]
    fn extracts_nested_y_js_click_url() {
        let raw = "/y.js?u3=https%253A%252F%252Ftracker.example%252F%253Fld%253Dhttps%2525253A%2525252F%2525252Fexample.com%2525252Farticle";
        assert_eq!(extract_direct_url(raw), "https://example.com/article");
    }

    #[test]
    fn formats_results_exactly_like_the_reference() {
        let output = format_results("rust async", 2, &[SearchResultItem {
            title: "Async Rust".to_string(),
            url: "https://example.com/rust".to_string(),
            snippet: "A practical guide.".to_string(),
            display_url: "example.com/rust".to_string(),
        }]);

        assert_eq!(
            output,
            "Search results for \"rust async\":\n\nPage 2; showing 1 result(s)\n\n1. [Async Rust](https://example.com/rust)\n   A practical guide.\n   Source: example.com/rust\n"
        );
    }

    #[test]
    fn formats_empty_results_exactly_like_the_reference() {
        assert_eq!(format_results("missing", 1, &[]), "No results found.");
    }
}
