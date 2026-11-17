//! Web tools: search the public web and read a page without a browser.
//! Port of `brain/nerv/tools/web.py` — the DuckDuckGo HTML result extraction
//! uses `scraper` (html5ever) where python used `html.parser`.

use serde_json::json;
use std::time::Duration;

use super::helpers::{truncate, DEFAULT_HTTP_TIMEOUT, MAX_TOOL_OUTPUT_CHARS};
use super::registry::{ToolDefinition, ToolResult};

const SEARCH_ENDPOINT: &str = "https://html.duckduckgo.com/html/";
const USER_AGENT: &str = "Nerv/0.1 (+https://github.com/IShinji/Nerv)";

const RESULT_LINK_AND_SNIPPET: &str =
    "a.result__a, a.result__snippet, div.result__snippet, span.result__snippet";

/// Normalize DuckDuckGo redirect URLs into direct targets.
fn normalize_search_result_url(url: &str) -> String {
    if url.is_empty() {
        return url.to_string();
    }

    let mut url = url.to_string();
    if let Some(rest) = url.strip_prefix("//") {
        url = format!("https://{rest}");
    } else if let Some(rest) = url.strip_prefix('/') {
        url = format!("https://html.duckduckgo.com/{rest}");
    }

    let Ok(parsed) = url::Url::parse(&url) else {
        return url;
    };
    if parsed
        .host_str()
        .is_some_and(|host| host.contains("duckduckgo.com"))
        && parsed.path().starts_with("/l/")
    {
        for (key, value) in parsed.query_pairs() {
            if key == "uddg" {
                return percent_decode(&value);
            }
        }
    }
    url
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&value[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Whether this tree node sits inside a script/style/noscript subtree.
fn in_skip_subtree(node: ego_tree::NodeRef<'_, scraper::Node>) -> bool {
    let mut ancestor = node.parent();
    while let Some(current) = ancestor {
        if let Some(element) = current.value().as_element() {
            if matches!(element.name(), "script" | "style" | "noscript") {
                return true;
            }
        }
        ancestor = current.parent();
    }
    false
}

/// Extract a bounded set of search results from DDG HTML, in document order —
/// a `result__a` link opens a result, the following snippet attaches to it
/// (mirroring the python parser's sequential pairing).
fn parse_search_results(html: &str, max_results: usize) -> Vec<(String, String, String)> {
    use scraper::{Html, Selector};

    let document = Html::parse_document(html);
    let selector = Selector::parse(RESULT_LINK_AND_SNIPPET).expect("static selector");

    let mut results: Vec<(String, String, String)> = Vec::new();
    for element in document.select(&selector) {
        let class = element.value().attr("class").unwrap_or("");
        if element.value().name() == "a" && class.contains("result__a") {
            let href = element.value().attr("href").unwrap_or("").to_string();
            let title = element
                .text()
                .collect::<Vec<_>>()
                .join(" ")
                .trim()
                .to_string();
            let url = normalize_search_result_url(&href);
            if !title.is_empty() || !url.is_empty() {
                results.push((title, url, String::new()));
            }
        } else {
            let snippet = element
                .text()
                .collect::<Vec<_>>()
                .join(" ")
                .trim()
                .to_string();
            if !snippet.is_empty() {
                if let Some(last) = results.last_mut() {
                    if last.2.is_empty() {
                        last.2 = snippet;
                    }
                }
            }
        }
        if results.len() >= max_results {
            break;
        }
    }

    results
}

/// Extract title, text, and normalized links from an HTML page.
fn parse_page_content(html: &str, base_url: &str) -> (String, String, Vec<String>) {
    use scraper::{Html, Selector};

    let document = Html::parse_document(html);
    let title_sel = Selector::parse("title").expect("static selector");

    let title = document
        .select(&title_sel)
        .next()
        .map(|t| t.text().collect::<Vec<_>>().join(" ").trim().to_string())
        .unwrap_or_default();

    let base = url::Url::parse(base_url).ok();

    // One walk over the tree: harvest visible text and <a href> labels,
    // skipping script/style/noscript subtrees (the python parser's skip
    // depth behaviour).
    let mut text_parts: Vec<String> = Vec::new();
    let mut links: Vec<String> = Vec::new();

    for node in document.tree.nodes() {
        if in_skip_subtree(node) {
            continue;
        }
        if let Some(text_node) = node.value().as_text() {
            let cleaned = text_node
                .text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if !cleaned.is_empty() {
                text_parts.push(cleaned);
            }
        }
        if let Some(element) = node.value().as_element() {
            if element.name() != "a" {
                continue;
            }
            let Some(href) = element.attr("href") else {
                continue;
            };
            let label = node
                .descendants()
                .filter(|n| !in_skip_subtree(*n))
                .filter_map(|n| n.value().as_text().map(|t| t.text.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
                .trim()
                .to_string();
            if href.is_empty() || label.is_empty() {
                continue;
            }
            let normalized = base
                .as_ref()
                .and_then(|b| b.join(href).ok())
                .map(|u| u.to_string())
                .unwrap_or_else(|| href.to_string());
            links.push(format!("- {label}: {normalized}"));
        }
    }

    (title, text_parts.join(" ").trim().to_string(), links)
}

fn user_agent_headers() -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Ok(value) = reqwest::header::HeaderValue::from_str(USER_AGENT) {
        headers.insert(reqwest::header::USER_AGENT, value);
    }
    headers
}

pub fn web_search() -> ToolDefinition {
    ToolDefinition {
        name: "web_search",
        description: "Search the public web without using a browser. Provide a query and get back titles, URLs, and snippets.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "max_results": {"type": "integer"}
            },
            "required": ["query"]
        }),
        func: |_ctx, args| {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let max_results =
                args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
            Box::pin(async move { web_search_impl(&query, max_results).await })
        },
    }
}

async fn web_search_impl(query: &str, max_results: usize) -> ToolResult {
    if query.trim().is_empty() {
        return ToolResult::error("Error: web_search requires a non-empty query.");
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs_f64(DEFAULT_HTTP_TIMEOUT))
        .default_headers(user_agent_headers())
        .build()
    {
        Ok(client) => client,
        Err(e) => return ToolResult::error(format!("Search request failed: {e}")),
    };
    let response = match client
        .get(SEARCH_ENDPOINT)
        .query(&[("q", query)])
        .send()
        .await
        .and_then(|r| r.error_for_status())
    {
        Ok(response) => response,
        Err(e) => return ToolResult::error(format!("Search request failed: {e}")),
    };

    let html = response.text().await.unwrap_or_default();
    let results = parse_search_results(&html, max_results.max(1));
    if results.is_empty() {
        return ToolResult::ok(format!("No web search results found for: {query}"));
    }

    let mut lines = vec![format!("Web search results for \"{query}\":")];
    for (idx, (title, url, snippet)) in results.iter().enumerate() {
        lines.push(format!("{}. {title}", idx + 1));
        lines.push(format!("   URL: {url}"));
        if !snippet.is_empty() {
            lines.push(format!("   Snippet: {snippet}"));
        }
    }
    ToolResult::ok(truncate(
        &lines.join("\n"),
        MAX_TOOL_OUTPUT_CHARS,
        "web_search",
    ))
}

pub fn browser() -> ToolDefinition {
    ToolDefinition {
        name: "browser",
        description: "Fetch a webpage from a URL and return its title and readable text. Use this after web_search when you need page details.",
        requires_confirmation: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "max_chars": {"type": "integer"},
                "include_links": {"type": "boolean"}
            },
            "required": ["url"]
        }),
        func: |_ctx, args| {
            let url = args
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let max_chars =
                args.get("max_chars").and_then(|v| v.as_u64()).unwrap_or(4000) as usize;
            let include_links = args
                .get("include_links")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Box::pin(async move { browser_impl(&url, max_chars, include_links).await })
        },
    }
}

async fn browser_impl(url: &str, max_chars: usize, include_links: bool) -> ToolResult {
    let mut target_url = url.trim().to_string();
    if target_url.is_empty() {
        return ToolResult::error("Error: browser requires a URL.");
    }
    if !target_url.contains("://") {
        target_url = format!("https://{target_url}");
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs_f64(DEFAULT_HTTP_TIMEOUT))
        .default_headers(user_agent_headers())
        .build()
    {
        Ok(client) => client,
        Err(e) => return ToolResult::error(format!("Failed to fetch page: {e}")),
    };
    let response = match client
        .get(&target_url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
    {
        Ok(response) => response,
        Err(e) => return ToolResult::error(format!("Failed to fetch page: {e}")),
    };

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    let final_url = response.url().to_string();
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();

    let (title, text, links) = if content_type.contains("html") {
        parse_page_content(&body, &final_url)
    } else {
        (String::new(), body, Vec::new())
    };

    let mut lines = vec![
        format!("Fetched URL: {final_url}"),
        format!("Status: {status}"),
    ];
    if !title.is_empty() {
        lines.push(format!("Title: {title}"));
    }
    if !text.trim().is_empty() {
        lines.push("Content:".to_string());
        lines.push(truncate(text.trim(), max_chars, "browser"));
    }
    if include_links && !links.is_empty() {
        lines.push("Links:".to_string());
        lines.extend(links.into_iter().take(10));
    }

    ToolResult::ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddg_redirect_urls_normalize_to_targets() {
        assert_eq!(
            normalize_search_result_url(
                "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage"
            ),
            "https://example.com/page"
        );
        assert_eq!(
            normalize_search_result_url("/l/?uddg=https%3A%2F%2Fexample.com"),
            "https://example.com"
        );
        assert_eq!(
            normalize_search_result_url("https://example.com/direct"),
            "https://example.com/direct"
        );
    }

    #[test]
    fn search_results_pair_titles_with_their_snippets() {
        let html = r##"
        <html><body>
          <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.example">Result A</a>
          <a class="result__snippet" href="#">snippet A</a>
          <a class="result__a" href="https://b.example">Result B</a>
          <a class="result__snippet" href="#">snippet B</a>
        </body></html>"##;

        let results = parse_search_results(html, 5);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "Result A");
        assert_eq!(results[0].1, "https://a.example");
        assert_eq!(results[0].2, "snippet A");
        assert_eq!(results[1].2, "snippet B");
    }

    #[test]
    fn page_content_harvests_text_and_links_without_script_noise() {
        let html = r##"
        <html><head><title>Test Page</title>
        <script>var x = "this must not appear";</script>
        <style>.x { color: red }</style></head>
        <body>
          <p>visible body text</p>
          <a href="/next">the next page</a>
          <a href="https://external.example">external</a>
        </body></html>"##;

        let (title, text, links) = parse_page_content(html, "https://start.example/post/");
        assert_eq!(title, "Test Page");
        assert!(text.contains("visible body text"));
        assert!(!text.contains("this must not appear"));
        assert!(!text.contains("color: red"));
        assert!(links[0].contains("- the next page: https://start.example/next"));
        assert!(links[1].contains("https://external.example"));
    }

    #[tokio::test]
    async fn web_search_rejects_empty_queries() {
        let result = web_search_impl("", 5).await;
        assert!(result.is_error);
    }

    #[test]
    fn browser_tool_schemas_are_declared() {
        let registry = crate::tools::ToolRegistry;
        assert!(registry.get_tool("web_search").is_some());
        assert!(registry.get_tool("browser").is_some());
    }
}
