"""Web tools: search the public web and read a page without a browser."""

import logging
from html.parser import HTMLParser
from urllib.parse import parse_qs, unquote, urljoin, urlparse

import httpx

from nerv.tools._helpers import (
    DEFAULT_HTTP_TIMEOUT,
    SEARCH_ENDPOINT,
    _truncate,
)
from nerv.tools.registry import ToolResult, registry

logger = logging.getLogger(__name__)


class _SearchResultParser(HTMLParser):
    """Extract search results from DuckDuckGo HTML output."""

    def __init__(self) -> None:
        super().__init__()
        self.results: list[dict[str, str]] = []
        self._current_result: dict[str, str] | None = None
        self._capture_title = False
        self._capture_snippet = False
        self._title_parts: list[str] = []
        self._snippet_parts: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        attr_map = dict(attrs)
        class_names = attr_map.get("class", "") or ""

        if tag == "a" and "result__a" in class_names:
            self._current_result = {
                "title": "",
                "url": _normalize_search_result_url(attr_map.get("href", "") or ""),
                "snippet": "",
            }
            self._capture_title = True
            self._title_parts = []
            return

        if (
            self._current_result
            and tag in {"a", "div", "span"}
            and "result__snippet" in class_names
        ):
            self._capture_snippet = True
            self._snippet_parts = []

    def handle_endtag(self, tag: str) -> None:
        if tag == "a" and self._capture_title and self._current_result is not None:
            self._capture_title = False
            self._current_result["title"] = " ".join(self._title_parts).strip()
            if self._current_result["title"] or self._current_result["url"]:
                self.results.append(self._current_result)
            self._current_result = None
            self._title_parts = []
            return

        if tag in {"a", "div", "span"} and self._capture_snippet:
            self._capture_snippet = False
            snippet = " ".join(self._snippet_parts).strip()
            if snippet and self.results and not self.results[-1]["snippet"]:
                self.results[-1]["snippet"] = snippet
            self._snippet_parts = []

    def handle_data(self, data: str) -> None:
        text = " ".join(data.split())
        if not text:
            return

        if self._capture_title:
            self._title_parts.append(text)
        elif self._capture_snippet:
            self._snippet_parts.append(text)


class _ReadablePageParser(HTMLParser):
    """Extract readable content from an HTML page."""

    def __init__(self) -> None:
        super().__init__()
        self.title_parts: list[str] = []
        self.text_parts: list[str] = []
        self.links: list[tuple[str, str]] = []
        self._in_title = False
        self._skip_depth = 0
        self._capture_link = False
        self._link_href = ""
        self._link_text_parts: list[str] = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        attr_map = dict(attrs)
        if tag in {"script", "style", "noscript"}:
            self._skip_depth += 1
            return
        if tag == "title":
            self._in_title = True
            return
        if tag == "a":
            self._capture_link = True
            self._link_href = attr_map.get("href", "") or ""
            self._link_text_parts = []

    def handle_endtag(self, tag: str) -> None:
        if tag in {"script", "style", "noscript"} and self._skip_depth > 0:
            self._skip_depth -= 1
            return
        if tag == "title":
            self._in_title = False
            return
        if tag == "a" and self._capture_link:
            link_text = " ".join(self._link_text_parts).strip()
            if self._link_href and link_text:
                self.links.append((self._link_href, link_text))
            self._capture_link = False
            self._link_href = ""
            self._link_text_parts = []

    def handle_data(self, data: str) -> None:
        if self._skip_depth > 0:
            return

        text = " ".join(data.split())
        if not text:
            return

        if self._in_title:
            self.title_parts.append(text)
        else:
            self.text_parts.append(text)

        if self._capture_link:
            self._link_text_parts.append(text)


def _normalize_search_result_url(url: str) -> str:
    """Normalize DuckDuckGo redirect URLs into direct targets."""
    if not url:
        return url

    if url.startswith("//"):
        url = "https:" + url
    elif url.startswith("/"):
        url = urljoin("https://html.duckduckgo.com", url)

    parsed = urlparse(url)
    if "duckduckgo.com" in parsed.netloc and parsed.path.startswith("/l/"):
        target = parse_qs(parsed.query).get("uddg")
        if target:
            return unquote(target[0])

    return url


def _parse_search_results(html: str, max_results: int) -> list[dict[str, str]]:
    """Extract a bounded set of search results from DDG HTML."""
    parser = _SearchResultParser()
    parser.feed(html)
    results = [
        result for result in parser.results if result.get("title") and result.get("url")
    ]
    return results[:max_results]


def _parse_page_content(html: str, base_url: str) -> tuple[str, str, list[str]]:
    """Extract title, text, and normalized links from an HTML page."""
    parser = _ReadablePageParser()
    parser.feed(html)

    title = " ".join(parser.title_parts).strip()
    text = " ".join(parser.text_parts).strip()

    links: list[str] = []
    for href, label in parser.links:
        normalized = urljoin(base_url, href)
        links.append(f"- {label}: {normalized}")

    return title, text, links


@registry.register(
    name="web_search",
    description=(
        "Search the public web without using a browser. "
        "Provide a query and get back titles, URLs, and snippets."
    ),
    requires_confirmation=False,
)
def web_search(query: str, max_results: int = 5) -> ToolResult:
    """Search the public web and return structured results."""
    if not query.strip():
        return ToolResult(
            content="Error: web_search requires a non-empty query.", is_error=True
        )

    try:
        with httpx.Client(
            timeout=DEFAULT_HTTP_TIMEOUT, follow_redirects=True
        ) as client:
            response = client.get(
                SEARCH_ENDPOINT,
                params={"q": query},
                headers={"User-Agent": "Nerv/0.1 (+https://github.com/IShinji/Nerv)"},
            )
            response.raise_for_status()
    except httpx.HTTPError as e:
        return ToolResult(content=f"Search request failed: {e}", is_error=True)

    results = _parse_search_results(response.text, max(1, max_results))
    if not results:
        return ToolResult(content=f"No web search results found for: {query}")

    lines = [f'Web search results for "{query}":']
    for idx, result in enumerate(results, start=1):
        lines.append(f"{idx}. {result['title']}")
        lines.append(f"   URL: {result['url']}")
        if result.get("snippet"):
            lines.append(f"   Snippet: {result['snippet']}")

    return ToolResult(content=_truncate("\n".join(lines)))


@registry.register(
    name="browser",
    description=(
        "Fetch a webpage from a URL and return its title and readable text. "
        "Use this after web_search when you need page details."
    ),
    requires_confirmation=False,
)
def browser(url: str, max_chars: int = 4000, include_links: bool = False) -> ToolResult:
    """Fetch webpage content and extract readable text."""
    target_url = url.strip()
    if not target_url:
        return ToolResult(content="Error: browser requires a URL.", is_error=True)

    if not urlparse(target_url).scheme:
        target_url = "https://" + target_url

    try:
        with httpx.Client(
            timeout=DEFAULT_HTTP_TIMEOUT, follow_redirects=True
        ) as client:
            response = client.get(
                target_url,
                headers={"User-Agent": "Nerv/0.1 (+https://github.com/IShinji/Nerv)"},
            )
            response.raise_for_status()
    except httpx.HTTPError as e:
        return ToolResult(content=f"Failed to fetch page: {e}", is_error=True)

    content_type = response.headers.get("content-type", "").lower()
    final_url = str(response.url)

    if "html" in content_type:
        title, text, links = _parse_page_content(response.text, final_url)
    else:
        title = ""
        text = response.text
        links = []

    lines = [f"Fetched URL: {final_url}", f"Status: {response.status_code}"]
    if title:
        lines.append(f"Title: {title}")
    if text.strip():
        lines.append("Content:")
        lines.append(_truncate(text.strip(), max_chars))
    if include_links and links:
        lines.append("Links:")
        lines.extend(links[:10])

    return ToolResult(content="\n".join(lines))
