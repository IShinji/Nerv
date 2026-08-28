"""Builtin Tools for agents."""

import datetime as dt
import json
import logging
import pathlib
import platform
import shutil
import subprocess
import tempfile
import time
from html.parser import HTMLParser
from urllib.parse import parse_qs, unquote, urljoin, urlparse
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

import httpx

from nerv.tools.registry import ToolResult, registry

logger = logging.getLogger(__name__)

TIMEZONE_ALIASES = {
    "utc": "UTC",
    "gmt": "UTC",
    "seattle": "America/Los_Angeles",
    "los angeles": "America/Los_Angeles",
    "san francisco": "America/Los_Angeles",
    "new york": "America/New_York",
    "beijing": "Asia/Shanghai",
    "shanghai": "Asia/Shanghai",
    "tokyo": "Asia/Tokyo",
    "london": "Europe/London",
}
DEFAULT_HTTP_TIMEOUT = 20.0
MAX_TOOL_OUTPUT_CHARS = 5000
SEARCH_ENDPOINT = "https://html.duckduckgo.com/html/"
DEFAULT_SCREENSHOT_NAME = "nerv-screenshot"
CHROME_APP_NAME = "Google Chrome"
SPECIAL_KEY_CODES = {
    "enter": 36,
    "return": 36,
    "tab": 48,
    "space": 49,
    "escape": 53,
    "esc": 53,
    "left": 123,
    "right": 124,
    "down": 125,
    "up": 126,
}
APPLE_MODIFIERS = {
    "command": "command down",
    "cmd": "command down",
    "shift": "shift down",
    "option": "option down",
    "alt": "option down",
    "control": "control down",
    "ctrl": "control down",
}


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


def _resolve_path(path: str) -> pathlib.Path:
    """Resolve a path for tool access."""
    candidate = pathlib.Path(path).expanduser()
    if candidate.is_absolute():
        return candidate
    return (_get_project_root() / candidate).resolve()


def _get_project_root() -> pathlib.Path:
    """Find the Nerv project root from the current working directory."""
    current = pathlib.Path.cwd().resolve()
    for candidate in [current, *current.parents]:
        if (candidate / "agents").exists() or (candidate / "core").exists():
            return candidate
    return current


def _truncate(text: str, limit: int = MAX_TOOL_OUTPUT_CHARS) -> str:
    """Truncate large tool output for context safety."""
    if len(text) <= limit:
        return text
    return text[:limit] + "\n...[truncated]"


def _parse_csv_or_lines(value: str) -> list[str]:
    """Parse comma- or newline-separated text into a clean string list."""
    if not value.strip():
        return []
    normalized = value.replace("\r", "\n").replace(",", "\n")
    return [item.strip() for item in normalized.split("\n") if item.strip()]


def _run_subprocess(args: list[str]) -> subprocess.CompletedProcess[str]:
    """Run a subprocess and capture UTF-8 output."""
    return subprocess.run(args, capture_output=True, text=True)


def _run_applescript(lines: list[str]) -> subprocess.CompletedProcess[str]:
    """Run an AppleScript program built from individual lines."""
    args = ["osascript"]
    for line in lines:
        args.extend(["-e", line])
    return _run_subprocess(args)


def _escape_applescript_text(text: str) -> str:
    """Escape text embedded into AppleScript string literals."""
    return text.replace("\\", "\\\\").replace('"', '\\"')


def _normalize_modifiers(modifiers: str) -> list[str]:
    """Convert a free-form modifiers string into AppleScript modifier flags."""
    normalized = []
    for item in modifiers.replace("+", ",").split(","):
        token = item.strip().lower()
        if not token:
            continue
        if token in APPLE_MODIFIERS:
            normalized.append(APPLE_MODIFIERS[token])
    return normalized


def _resolve_capture_path(output_path: str) -> pathlib.Path:
    """Resolve a screenshot output path, creating a temp path when omitted."""
    if output_path.strip():
        path = _resolve_path(output_path)
        path.parent.mkdir(parents=True, exist_ok=True)
        return path

    timestamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    return (
        pathlib.Path(tempfile.gettempdir())
        / f"{DEFAULT_SCREENSHOT_NAME}-{timestamp}.png"
    )


def _extract_text_from_image(image_path: pathlib.Path) -> str:
    """Best-effort OCR for macOS screenshots using Vision via Swift."""
    if platform.system().lower() != "darwin":
        return ""

    script = """
import AppKit
import Foundation
import Vision

let imagePath = CommandLine.arguments[1]
let url = URL(fileURLWithPath: imagePath)

guard
    let image = NSImage(contentsOf: url),
    let tiff = image.tiffRepresentation,
    let bitmap = NSBitmapImageRep(data: tiff),
    let cgImage = bitmap.cgImage
else {
    fputs("Failed to load image\\n", stderr)
    exit(1)
}

let request = VNRecognizeTextRequest()
request.recognitionLevel = .accurate
request.usesLanguageCorrection = true

let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
do {
    try handler.perform([request])
    let observations = request.results as? [VNRecognizedTextObservation] ?? []
    for observation in observations {
        if let candidate = observation.topCandidates(1).first {
            print(candidate.string)
        }
    }
} catch {
    fputs("\\(error)\\n", stderr)
    exit(2)
}
""".strip()

    result = _run_subprocess(["swift", "-e", script, str(image_path)])
    if result.returncode != 0:
        return ""
    return result.stdout.strip()


def _build_hotkey_applescript(key: str, modifiers: str) -> str:
    """Build AppleScript for a key press or hotkey."""
    normalized_key = key.strip().lower()
    modifier_flags = _normalize_modifiers(modifiers)
    using_clause = ""
    if modifier_flags:
        using_clause = f" using {{{', '.join(modifier_flags)}}}"

    if normalized_key in SPECIAL_KEY_CODES:
        return (
            'tell application "System Events" '
            f"to key code {SPECIAL_KEY_CODES[normalized_key]}{using_clause}"
        )

    if len(key) == 1:
        escaped_key = _escape_applescript_text(key)
        return (
            'tell application "System Events" '
            f'to keystroke "{escaped_key}"{using_clause}'
        )

    raise ValueError(
        "Unsupported key name. Use a single character or a named key like "
        "enter, tab, escape, up, down, left, or right."
    )


def _ensure_url(target_url: str) -> str:
    """Normalize a user URL into an absolute browser URL."""
    normalized = target_url.strip()
    if not normalized:
        return normalized
    if not urlparse(normalized).scheme:
        normalized = "https://" + normalized
    return normalized


def _ensure_chrome_running() -> ToolResult | None:
    """Validate that Chrome automation is available on the current host."""
    if platform.system().lower() != "darwin":
        return ToolResult(
            content=(
                "chrome_browser MVP is currently implemented for macOS only. "
                f"Current platform: {platform.system()}"
            ),
            is_error=True,
        )

    chrome_path = pathlib.Path("/Applications") / f"{CHROME_APP_NAME}.app"
    if not chrome_path.exists():
        return ToolResult(
            content=f"{CHROME_APP_NAME} is not installed in /Applications.",
            is_error=True,
        )

    # Auto-enable "Allow JavaScript from Apple Events" so users don't need
    # to manually toggle View > Developer > Allow JavaScript from Apple Events.
    _ensure_chrome_applescript_enabled()

    return None


_CHROME_AS_ENABLED = False  # Module-level flag to avoid repeated subprocess calls


def _ensure_chrome_applescript_enabled() -> None:
    """Ensure Chrome allows JavaScript execution via AppleScript.

    The 'Allow JavaScript from Apple Events' toggle in Chrome can only be
    changed via the Chrome UI menu (View > Developer). We first probe whether
    JS works; if not, we attempt to toggle the menu via macOS UI scripting.
    If that fails (no Accessibility permission), we show a clear notification.
    """
    global _CHROME_AS_ENABLED
    if _CHROME_AS_ENABLED:
        return

    # Probe: try to execute a trivial JS snippet in the current Chrome instance
    probe = _run_applescript(
        [
            f'tell application "{CHROME_APP_NAME}"',
            "if (count of windows) = 0 then",
            "  make new window",
            "  delay 1",
            "end if",
            "try",
            '  return execute active tab of front window javascript "1+1"',
            "on error",
            '  return "BLOCKED"',
            "end try",
            "end tell",
        ]
    )

    if probe.returncode == 0 and probe.stdout.strip() != "BLOCKED":
        _CHROME_AS_ENABLED = True
        return

    # JS blocked → try to toggle the menu item via UI scripting (requires Accessibility permission)
    logger.info(
        "Chrome AppleScript JS is blocked. Attempting to enable via menu toggle..."
    )

    toggle_result = _run_applescript(
        [
            f'tell application "{CHROME_APP_NAME}" to activate',
            "delay 0.5",
            'tell application "System Events"',
            f'  tell process "{CHROME_APP_NAME}"',
            '    click menu item "Allow JavaScript from Apple Events" of menu "Developer" of menu item "Developer" of menu "View" of menu bar 1',
            "  end tell",
            "end tell",
            "delay 1",
        ]
    )

    if toggle_result.returncode == 0:
        logger.info("Successfully toggled Chrome JS permission via menu.")
        _CHROME_AS_ENABLED = True
        return

    # UI scripting also failed (no Accessibility permission) → show clear error
    logger.warning(
        "Could not auto-enable Chrome JS permission. "
        "Please enable it manually: Chrome menu → View → Developer → Allow JavaScript from Apple Events"
    )
    # Open Chrome and show a macOS notification to guide the user
    subprocess.run(
        [
            "osascript",
            "-e",
            "display notification "
            '"Please enable: Chrome → View → Developer → Allow JavaScript from Apple Events" '
            'with title "Nerv Setup Required"',
        ],
        capture_output=True,
        timeout=5,
    )


def _chrome_run_script(lines: list[str]) -> ToolResult:
    """Execute AppleScript against Google Chrome and return stdout."""
    precheck = _ensure_chrome_running()
    if precheck:
        return precheck

    result = _run_applescript(lines)
    if result.returncode != 0:
        stderr = result.stderr.strip() or result.stdout.strip() or "unknown error"
        return ToolResult(content=f"Chrome automation failed: {stderr}", is_error=True)

    return ToolResult(content=result.stdout.strip())


def _chrome_get_state() -> ToolResult:
    """Read basic state from the active Chrome tab."""
    result = _chrome_run_script(
        [
            f'tell application "{CHROME_APP_NAME}"',
            "activate",
            'if (count of windows) = 0 then return "NO_WINDOW"',
            "set tab_title to title of active tab of front window",
            "set tab_url to URL of active tab of front window",
            "set tab_loading to (loading of active tab of front window as text)",
            "return tab_title & linefeed & tab_url & linefeed & tab_loading",
            "end tell",
        ]
    )
    if result.is_error:
        return result

    if result.content == "NO_WINDOW":
        return ToolResult(content="Chrome has no open window.", is_error=True)

    title, url, loading = (result.content.splitlines() + ["", "", ""])[:3]
    payload = {
        "title": title,
        "url": url,
        "loading": loading.lower() == "true",
    }
    return ToolResult(content=json.dumps(payload, ensure_ascii=False))


def _chrome_execute_javascript(script: str) -> ToolResult:
    """Run JavaScript inside the active Chrome tab."""
    escaped_script = _escape_applescript_text(script)
    result = _chrome_run_script(
        [
            f'tell application "{CHROME_APP_NAME}"',
            "activate",
            'if (count of windows) = 0 then return "NO_WINDOW"',
            f'return execute active tab of front window javascript "{escaped_script}"',
            "end tell",
        ]
    )
    if result.is_error:
        return result
    if result.content == "NO_WINDOW":
        return ToolResult(content="Chrome has no open window.", is_error=True)
    return result


def _chrome_page_text(max_chars: int) -> ToolResult:
    """Extract readable page text from the active tab."""
    script = """
(() => {
  const text = document.body ? document.body.innerText : "";
  return text || "";
})()
""".strip()
    result = _chrome_execute_javascript(script)
    if result.is_error:
        return result
    return ToolResult(content=_truncate(result.content.strip(), max_chars))


def _build_browser_interactive_arguments(
    action: str,
    url: str,
    text: str,
    script: str,
    wait_for: str,
    timeout_secs: int,
    poll_interval_secs: float,
    max_chars: int,
) -> tuple[dict[str, object] | None, ToolResult | None]:
    """Validate and normalize browser-interactive action arguments."""
    normalized_action = action.strip().lower()

    if normalized_action == "open_url":
        target_url = _ensure_url(url)
        if not target_url:
            return None, ToolResult(content="open_url requires `url`.", is_error=True)
        return {"url": target_url}, None

    if normalized_action == "get_state":
        return {}, None

    if normalized_action == "get_page_text":
        return {"max_chars": max_chars}, None

    if normalized_action == "run_javascript":
        if not script.strip():
            return None, ToolResult(
                content="run_javascript requires `script`.", is_error=True
            )
        return {"script": script}, None

    if normalized_action == "fill_prompt":
        if not text:
            return None, ToolResult(
                content="fill_prompt requires `text`.", is_error=True
            )
        return {"text": text}, None

    if normalized_action == "submit_prompt":
        return {}, None

    if normalized_action == "wait_for_idle":
        return {
            "timeout_secs": max(1, timeout_secs),
            "poll_interval_secs": max(0.1, poll_interval_secs),
        }, None

    if normalized_action == "wait_for_text":
        if not wait_for.strip():
            return None, ToolResult(
                content="wait_for_text requires `wait_for`.", is_error=True
            )
        return {
            "wait_for": wait_for,
            "timeout_secs": max(1, timeout_secs),
            "poll_interval_secs": max(0.1, poll_interval_secs),
            "max_chars": max_chars,
        }, None

    return None, ToolResult(
        content=(
            "Unsupported browser_interactive action. Use one of: open_url, get_state, "
            "get_page_text, fill_prompt, submit_prompt, wait_for_text, "
            "wait_for_idle, run_javascript."
        ),
        is_error=True,
    )


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


# -----------------------------------------------------------------------------
# File System Tools
# -----------------------------------------------------------------------------


@registry.register(
    name="file_io",
    description=(
        "Inspect files and directories. operation can be read, list, or search. "
        "For writes, use write_file_full because writes are higher risk."
    ),
    requires_confirmation=False,
)
def file_io(
    operation: str,
    path: str,
    query: str = "",
    limit: int = 50,
) -> ToolResult:
    """Unified read/list/search file tool."""
    target = _resolve_path(path)
    normalized_op = operation.strip().lower()

    if normalized_op == "read":
        return read_file(str(target))

    if normalized_op == "list":
        if not target.exists():
            return ToolResult(
                content=f"Error: Path {target} does not exist.", is_error=True
            )
        if not target.is_dir():
            return ToolResult(
                content=f"Error: {target} is not a directory.", is_error=True
            )

        entries = sorted(
            target.iterdir(), key=lambda item: (item.is_file(), item.name.lower())
        )
        rendered = []
        for entry in entries[: max(1, limit)]:
            kind = "dir" if entry.is_dir() else "file"
            rendered.append(f"[{kind}] {entry.name}")

        if len(entries) > limit:
            rendered.append("...[truncated]")

        return ToolResult(content=f"Listing for {target}:\n" + "\n".join(rendered))

    if normalized_op == "search":
        if not query.strip():
            return ToolResult(
                content="Error: search requires a non-empty query.", is_error=True
            )
        return grep_search(str(target), query)

    if normalized_op in {"write", "append", "delete", "move"}:
        return ToolResult(
            content=(
                f"Unsupported file_io operation '{operation}'. "
                "Use write_file_full for writes because write operations require confirmation."
            ),
            is_error=True,
        )

    return ToolResult(
        content=f"Unsupported file_io operation '{operation}'. Use read, list, or search.",
        is_error=True,
    )


@registry.register(
    name="current_time",
    description=(
        "Get the current date and time. Use an IANA timezone like "
        "America/Los_Angeles or a common city like Seattle. "
        "Use this instead of guessing current time/date."
    ),
    requires_confirmation=False,
)
def current_time(timezone: str = "local") -> ToolResult:
    """Return the current date and time for the requested timezone."""
    normalized = timezone.strip()

    try:
        if not normalized or normalized.lower() == "local":
            current = dt.datetime.now().astimezone()
            timezone_label = str(current.tzinfo)
        else:
            zone_name = TIMEZONE_ALIASES.get(normalized.lower(), normalized)
            current = dt.datetime.now(ZoneInfo(zone_name))
            timezone_label = zone_name
    except ZoneInfoNotFoundError:
        return ToolResult(
            content=(
                f"Unknown timezone '{timezone}'. Please use an IANA timezone like "
                "America/Los_Angeles."
            ),
            is_error=True,
        )

    return ToolResult(
        content=(
            f"Current time in {timezone_label}: "
            f"{current.strftime('%Y-%m-%d %H:%M:%S %Z')}"
        )
    )


@registry.register(
    name="read_file",
    description="Read the exact contents of a file at a given absolute path.",
    requires_confirmation=False,
)
def read_file(absolute_path: str) -> ToolResult:
    """Read a file."""
    path = pathlib.Path(absolute_path)
    if not path.exists():
        return ToolResult(
            content=f"Error: File {absolute_path} does not exist.", is_error=True
        )
    if not path.is_file():
        return ToolResult(
            content=f"Error: {absolute_path} is not a file.", is_error=True
        )

    try:
        content = path.read_text(encoding="utf-8")
        return ToolResult(content=content)
    except Exception as e:
        return ToolResult(content=f"Error reading file: {e}", is_error=True)


@registry.register(
    name="write_file_full",
    description="Overwrite a file entirely with new content. Use carefully as this deletes old content.",
    requires_confirmation=True,
)
def write_file_full(absolute_path: str, content: str) -> ToolResult:
    """Overwrite a file entirely."""
    path = pathlib.Path(absolute_path)
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
        return ToolResult(content=f"Successfully completely wrote to {absolute_path}.")
    except Exception as e:
        return ToolResult(content=f"Error writing file: {e}", is_error=True)


@registry.register(
    name="grep_search",
    description="Search for a text pattern inside a directory or file using grep logic.",
    requires_confirmation=False,
)
def grep_search(search_path: str, query: str) -> ToolResult:
    """Search for a string in files."""
    path = pathlib.Path(search_path)
    if not path.exists():
        return ToolResult(
            content=f"Error: Path {search_path} does not exist.", is_error=True
        )

    try:
        # For MVP we use builtin grep via subprocess. No regex for MVP, just exact match
        args = ["grep", "-rnI", query, str(path)]
        result = subprocess.run(args, capture_output=True, text=True)

        if result.returncode == 0:
            out = result.stdout
            if len(out) > 5000:
                out = out[:5000] + "\\n...[truncated because it's too long]"
            return ToolResult(content=f"Found matches:\\n{out}")
        elif result.returncode == 1:
            return ToolResult(content="No matches found.")
        else:
            return ToolResult(
                content=f"Error running search: {result.stderr}", is_error=True
            )
    except Exception as e:
        return ToolResult(content=f"Error searching: {e}", is_error=True)


# -----------------------------------------------------------------------------
# Web Tools
# -----------------------------------------------------------------------------


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


@registry.register(
    name="list_workflows",
    description=(
        "List shared workflows relevant to an agent or task domain. "
        "Use this before drafting a new workflow."
    ),
    requires_confirmation=False,
)
def list_workflows(agent_name: str = "") -> ToolResult:
    """List approved shared workflows."""
    from nerv.workflows.registry import WorkflowRegistry

    project_root = _get_project_root()
    workflow_registry = WorkflowRegistry(project_root)
    workflows = workflow_registry.list_workflows()
    if agent_name.strip():
        filtered = []
        target = agent_name.strip().lower()
        for workflow in workflows:
            owners = {owner.lower() for owner in workflow.owner_agents}
            if target in owners:
                filtered.append(workflow)
        workflows = filtered

    if not workflows:
        return ToolResult(content="No shared workflows found.")

    lines = ["Shared workflows:"]
    for workflow in workflows:
        owners = (
            ", ".join(workflow.owner_agents) if workflow.owner_agents else "all agents"
        )
        lines.append(f"- {workflow.name}: {workflow.description}")
        lines.append(f"  Owners: {owners}")
        if workflow.tools:
            lines.append(f"  Tools: {', '.join(workflow.tools)}")
    return ToolResult(content="\n".join(lines))


@registry.register(
    name="get_workflow",
    description="Read a shared workflow definition by name, including steps and success criteria.",
    requires_confirmation=False,
)
def get_workflow(name: str) -> ToolResult:
    """Get details for a specific shared workflow."""
    from nerv.workflows.registry import WorkflowRegistry

    workflow = WorkflowRegistry(_get_project_root()).find(name)
    if workflow is None:
        return ToolResult(content=f"Workflow not found: {name}", is_error=True)

    lines = [f"Workflow: {workflow.name}", workflow.description]
    if workflow.steps:
        lines.append("Steps:")
        lines.extend(f"- {step}" for step in workflow.steps)
    if workflow.success_criteria:
        lines.append("Success criteria:")
        lines.extend(f"- {item}" for item in workflow.success_criteria)
    return ToolResult(content="\n".join(lines))


@registry.register(
    name="propose_workflow",
    description=(
        "Draft a new workflow proposal and send it to the review queue. "
        "Input list fields as comma-separated or newline-separated text."
    ),
    requires_confirmation=False,
)
def propose_workflow(
    name: str,
    description: str,
    tags: str = "",
    owner_agents: str = "",
    tools: str = "",
    steps: str = "",
    success_criteria: str = "",
) -> ToolResult:
    """Create a pending workflow review item."""
    from nerv.workflows.models import WorkflowDefinition
    from nerv.workflows.registry import ReviewQueue

    workflow = WorkflowDefinition(
        name=name.strip(),
        description=description.strip(),
        tags=_parse_csv_or_lines(tags),
        owner_agents=_parse_csv_or_lines(owner_agents),
        tools=_parse_csv_or_lines(tools),
        steps=_parse_csv_or_lines(steps),
        success_criteria=_parse_csv_or_lines(success_criteria),
        created_by="agent",
        review_status="pending",
    )

    review_item = ReviewQueue(_get_project_root()).submit_workflow(
        workflow,
        proposed_by="agent",
    )
    return ToolResult(
        content=(
            f"Workflow proposal queued for review: {review_item.review_id}\n"
            f"Name: {review_item.workflow.name}\n"
            'Ask the user to review it with "show workflow reviews", '
            f'"approve workflow {review_item.review_id}", or '
            f'"reject workflow {review_item.review_id}".'
        )
    )


@registry.register(
    name="execute_workflow",
    description=(
        "Execute a named shared workflow. This delegates control to the Workflow Executor engine "
        "which will sequentially perform the steps and return the final structurally extracted response."
    ),
    requires_confirmation=False,
)
async def execute_workflow(workflow_name: str, inputs: str) -> ToolResult:
    """Execute a shared workflow via the native WorkflowExecutor."""
    # We must fetch the orchestrator from ipc to inject it into the executor
    # To avoid circular import, we fetch it locally
    from nerv.ipc import _get_orchestrator
    from nerv.workflows.executor import WorkflowExecutor
    from nerv.workflows.registry import WorkflowRegistry

    registry_obj = WorkflowRegistry(_get_project_root())
    workflow = registry_obj.find(workflow_name.strip())

    if not workflow:
        return ToolResult(
            content=f"Error: Shared workflow '{workflow_name}' not found. List workflows first.",
            is_error=True,
        )

    if not workflow.steps:
        return ToolResult(
            content=f"Error: Workflow '{workflow.name}' has no defined steps.",
            is_error=True,
        )

    orchestrator = _get_orchestrator()
    executor = WorkflowExecutor(orchestrator)

    try:
        final_result = await executor.execute(workflow, inputs)
        return ToolResult(
            content=f"Workflow executed successfully.\\nResult context:\\n{final_result}"
        )
    except Exception as e:
        return ToolResult(content=f"Workflow execution failed: {e}", is_error=True)


@registry.register(
    name="list_skills",
    description="List shared skills relevant to an agent or task domain.",
    requires_confirmation=False,
)
def list_skills(agent_name: str = "") -> ToolResult:
    """List available skills."""
    from nerv.skills.registry import SkillRegistry

    skills = SkillRegistry(_get_project_root()).list_skills()
    if agent_name.strip():
        target = agent_name.strip().lower()
        skills = [
            skill
            for skill in skills
            if target in {owner.lower() for owner in skill.owner_agents}
        ]

    if not skills:
        return ToolResult(content="No skills found.")

    lines = ["Skills:"]
    for skill in skills:
        lines.append(f"- {skill.name}: {skill.description}")
        if skill.recommended_workflows:
            lines.append(
                "  Recommended workflows: " + ", ".join(skill.recommended_workflows)
            )
    return ToolResult(content="\n".join(lines))


@registry.register(
    name="get_skill",
    description="Read the details of a specific skill by name.",
    requires_confirmation=False,
)
def get_skill(name: str) -> ToolResult:
    """Get a skill description and body."""
    from nerv.skills.registry import SkillRegistry

    skill = SkillRegistry(_get_project_root()).find(name)
    if skill is None:
        return ToolResult(content=f"Skill not found: {name}", is_error=True)

    body = _truncate(skill.body.strip(), 3000)
    lines = [f"Skill: {skill.name}", skill.description]
    if skill.recommended_workflows:
        lines.append("Recommended workflows: " + ", ".join(skill.recommended_workflows))
    if body:
        lines.append("Body:")
        lines.append(body)
    return ToolResult(content="\n".join(lines))


@registry.register(
    name="list_review_queue",
    description="List pending or historical workflow review items.",
    requires_confirmation=False,
)
def list_review_queue(status: str = "pending") -> ToolResult:
    """List review items for workflows."""
    from nerv.workflows.registry import ReviewQueue

    items = ReviewQueue(_get_project_root()).list_reviews(status=status.strip())
    if not items:
        return ToolResult(
            content=f"No workflow review items found for status: {status}"
        )

    lines = [f"Workflow review items ({status}):"]
    for item in items:
        lines.append(
            f"- {item.review_id}: {item.workflow.name} [{item.status}] - {item.workflow.description}"
        )
    return ToolResult(content="\n".join(lines))


@registry.register(
    name="mcp_presets",
    description=(
        "List builtin MCP presets or render a config snippet for one preset. "
        "Use this to configure a known MCP server such as the official Chrome "
        "DevTools MCP integration."
    ),
    requires_confirmation=False,
)
def mcp_presets(preset_name: str = "") -> ToolResult:
    """List builtin MCP presets or render one preset snippet."""
    from nerv.mcp.presets import (
        get_builtin_mcp_preset,
        list_builtin_mcp_presets,
        render_mcp_preset_snippet,
    )

    normalized = preset_name.strip()
    if not normalized:
        presets = list_builtin_mcp_presets()
        if not presets:
            return ToolResult(content="No builtin MCP presets are available.")

        lines = ["Builtin MCP presets:"]
        for preset in presets:
            capabilities = (
                ", ".join(preset.capabilities) if preset.capabilities else "none"
            )
            lines.append(f"- {preset.name}: {preset.description}")
            lines.append(f"  Capabilities: {capabilities}")
            lines.append(f"  Source: {preset.source_url}")
        return ToolResult(content="\n".join(lines))

    preset = get_builtin_mcp_preset(normalized)
    if preset is None:
        return ToolResult(content=f"Unknown MCP preset: {preset_name}", is_error=True)

    lines = [
        f"Preset: {preset.name}",
        preset.description,
        f"Source: {preset.source_url}",
    ]
    if preset.default_action_map:
        lines.append(
            "Mapped actions: "
            + ", ".join(
                f"{action}->{tool_name}"
                for action, tool_name in sorted(preset.default_action_map.items())
            )
        )
    if preset.notes:
        lines.append("Notes:")
        lines.extend(f"- {note}" for note in preset.notes)
    lines.append("Config snippet:")
    lines.append(render_mcp_preset_snippet(preset))
    return ToolResult(content="\n".join(lines))


@registry.register(
    name="mcp_status",
    description=(
        "Inspect configured MCP servers, their health, bound capabilities, and "
        "available remote tools. Optionally pass a specific server name."
    ),
    requires_confirmation=False,
)
async def mcp_status(server_name: str = "") -> ToolResult:
    """Report MCP server configuration and health."""
    from nerv.mcp import get_mcp_manager
    from nerv.mcp.client import McpTransportError

    manager = await get_mcp_manager(_get_project_root())
    try:
        statuses = await manager.get_status(server_name)
    except McpTransportError as exc:
        return ToolResult(content=str(exc), is_error=True)

    if not statuses:
        return ToolResult(content="No MCP servers are configured.")

    lines = ["MCP server status:"]
    for status in statuses:
        health = "healthy" if status["healthy"] else "unhealthy"
        if not status["enabled"]:
            health = "disabled"

        lines.append(f"- {status['name']}: {health}")
        lines.append(f"  Transport: {status['transport']}")
        command = status["command"] or "(unset)"
        args = " ".join(status["args"]) if status["args"] else "(none)"
        lines.append(f"  Command: {command}")
        lines.append(f"  Args: {args}")
        if status["preset"]:
            lines.append(f"  Preset: {status['preset']}")
        capabilities = (
            ", ".join(status["capabilities"]) if status["capabilities"] else "none"
        )
        lines.append(f"  Capabilities: {capabilities}")
        if status["action_map"]:
            lines.append(
                "  Action map: "
                + ", ".join(
                    f"{action}->{tool_name}"
                    for action, tool_name in sorted(status["action_map"].items())
                )
            )
        if status["available_tools"]:
            lines.append("  Remote tools: " + ", ".join(status["available_tools"]))
        if status["error"]:
            lines.append(f"  Error: {status['error']}")

    return ToolResult(content="\n".join(lines))


@registry.register(
    name="browser_interactive",
    description=(
        "Interactive browser capability. Prefers a configured MCP browser provider "
        "and falls back to local chrome_browser when MCP is unavailable. "
        "Supported actions: open_url, get_state, get_page_text, fill_prompt, "
        "submit_prompt, wait_for_text, wait_for_idle, run_javascript."
    ),
    requires_confirmation=False,
)
async def browser_interactive(
    action: str,
    url: str = "",
    text: str = "",
    script: str = "",
    wait_for: str = "",
    timeout_secs: int = 30,
    poll_interval_secs: float = 1.0,
    max_chars: int = 4000,
) -> ToolResult:
    """Execute browser automation through MCP first, then native fallback."""
    arguments, validation_error = _build_browser_interactive_arguments(
        action,
        url,
        text,
        script,
        wait_for,
        timeout_secs,
        poll_interval_secs,
        max_chars,
    )
    if validation_error is not None:
        return validation_error

    normalized_action = action.strip().lower()

    try:
        from nerv.mcp import get_mcp_manager

        manager = await get_mcp_manager(_get_project_root())
        result = await manager.invoke_capability_action(
            "browser.interactive",
            normalized_action,
            arguments or {},
        )
        if result is not None:
            return result
    except Exception as exc:
        logger.warning(
            "browser_interactive MCP path failed for action %s, falling back to native: %s",
            normalized_action,
            exc,
        )

    return chrome_browser(
        action=normalized_action,
        url=str(arguments.get("url", "")) if arguments else "",
        text=str(arguments.get("text", "")) if arguments else "",
        script=str(arguments.get("script", "")) if arguments else "",
        wait_for=str(arguments.get("wait_for", "")) if arguments else "",
        timeout_secs=int(arguments.get("timeout_secs", timeout_secs))
        if arguments
        else timeout_secs,
        poll_interval_secs=float(
            arguments.get("poll_interval_secs", poll_interval_secs)
        )
        if arguments
        else poll_interval_secs,
        max_chars=int(arguments.get("max_chars", max_chars))
        if arguments
        else max_chars,
    )


@registry.register(
    name="chrome_browser",
    description=(
        "Automate Google Chrome for multi-step browser tasks. "
        "Supported actions: open_url, get_state, get_page_text, fill_prompt, "
        "submit_prompt, wait_for_text, wait_for_idle, run_javascript."
    ),
    requires_confirmation=False,
)
def chrome_browser(
    action: str,
    url: str = "",
    text: str = "",
    script: str = "",
    wait_for: str = "",
    timeout_secs: int = 30,
    poll_interval_secs: float = 1.0,
    max_chars: int = 4000,
) -> ToolResult:
    """Drive Chrome with a browser-scoped automation interface."""
    arguments, validation_error = _build_browser_interactive_arguments(
        action,
        url,
        text,
        script,
        wait_for,
        timeout_secs,
        poll_interval_secs,
        max_chars,
    )
    if validation_error is not None:
        return validation_error

    normalized_action = action.strip().lower()

    if normalized_action == "open_url":
        return _chrome_run_script(
            [
                f'tell application "{CHROME_APP_NAME}"',
                "activate",
                "if (count of windows) = 0 then make new window",
                f'set URL of active tab of front window to "{_escape_applescript_text(str(arguments["url"]))}"',
                f'return "Opened URL: {arguments["url"]}"',
                "end tell",
            ]
        )

    if normalized_action == "get_state":
        state = _chrome_get_state()
        if state.is_error:
            return state
        payload = json.loads(state.content)
        return ToolResult(
            content=(
                f"Title: {payload.get('title', '')}\n"
                f"URL: {payload.get('url', '')}\n"
                f"Loading: {payload.get('loading', False)}"
            )
        )

    if normalized_action == "get_page_text":
        return _chrome_page_text(int(arguments.get("max_chars", max_chars)))

    if normalized_action == "run_javascript":
        return _chrome_execute_javascript(str(arguments["script"]))

    if normalized_action == "fill_prompt":
        js = f"""
(() => {{
  const value = {json.dumps(str(arguments["text"]))};
  const selectors = [
    'textarea',
    'div[contenteditable="true"]',
    '[contenteditable="true"]',
    'input[type="text"]',
    'input:not([type])'
  ];
  const isVisible = (el) => {{
    if (!el) return false;
    const rect = el.getBoundingClientRect();
    const style = window.getComputedStyle(el);
    return rect.width > 0 && rect.height > 0 &&
      style.visibility !== 'hidden' &&
      style.display !== 'none';
  }};

  for (const selector of selectors) {{
    const candidates = Array.from(document.querySelectorAll(selector));
    const target = candidates.find(isVisible);
    if (!target) continue;

    target.focus();
    if (target.isContentEditable) {{
      target.innerText = value;
      target.dispatchEvent(new InputEvent('input', {{ bubbles: true, data: value }}));
    }} else {{
      target.value = value;
      target.dispatchEvent(new Event('input', {{ bubbles: true }}));
      target.dispatchEvent(new Event('change', {{ bubbles: true }}));
    }}
    return `filled:${{selector}}`;
  }}

  return "NO_EDITABLE_INPUT";
}})()
""".strip()
        result = _chrome_execute_javascript(js)
        if result.is_error:
            return result
        if result.content == "NO_EDITABLE_INPUT":
            return ToolResult(
                content="No editable prompt input was found on the current page.",
                is_error=True,
            )
        return ToolResult(content=f"Filled prompt using {result.content}.")

    if normalized_action == "submit_prompt":
        js = """
(() => {
  const selectors = [
    'button[aria-label*="Send"]',
    'button[aria-label*="send"]',
    'button[aria-label*="Submit"]',
    'button[aria-label*="submit"]',
    'button[data-testid*="send"]',
    'button[title*="Send"]',
    'button[title*="send"]'
  ];
  const isVisible = (el) => {
    if (!el) return false;
    const rect = el.getBoundingClientRect();
    const style = window.getComputedStyle(el);
    return rect.width > 0 && rect.height > 0 &&
      style.visibility !== 'hidden' &&
      style.display !== 'none' &&
      !el.disabled;
  };

  for (const selector of selectors) {
    const target = Array.from(document.querySelectorAll(selector)).find(isVisible);
    if (target) {
      target.click();
      return `clicked:${selector}`;
    }
  }

  const active = document.activeElement;
  if (active) {
    const options = { key: 'Enter', code: 'Enter', keyCode: 13, which: 13, bubbles: true };
    active.dispatchEvent(new KeyboardEvent('keydown', options));
    active.dispatchEvent(new KeyboardEvent('keypress', options));
    active.dispatchEvent(new KeyboardEvent('keyup', options));
    return 'pressed:Enter';
  }

  return 'NO_SUBMIT_CONTROL';
})()
""".strip()
        result = _chrome_execute_javascript(js)
        if result.is_error:
            return result
        if result.content == "NO_SUBMIT_CONTROL":
            return ToolResult(
                content="No submit control was found on the current page.",
                is_error=True,
            )
        return ToolResult(content=f"Submitted prompt using {result.content}.")

    if normalized_action == "wait_for_idle":
        deadline = time.time() + int(arguments.get("timeout_secs", timeout_secs))
        while time.time() < deadline:
            state = _chrome_get_state()
            if state.is_error:
                return state
            payload = json.loads(state.content)
            if not payload.get("loading", False):
                return ToolResult(content="Chrome tab is idle.")
            time.sleep(float(arguments.get("poll_interval_secs", poll_interval_secs)))
        return ToolResult(
            content="Timed out waiting for the Chrome tab to finish loading.",
            is_error=True,
        )

    if normalized_action == "wait_for_text":
        deadline = time.time() + int(arguments.get("timeout_secs", timeout_secs))
        while time.time() < deadline:
            page_text = _chrome_page_text(
                max_chars=max(12000, int(arguments.get("max_chars", max_chars)))
            )
            if page_text.is_error:
                return page_text
            if str(arguments["wait_for"]) in page_text.content:
                excerpt = _truncate(
                    page_text.content, int(arguments.get("max_chars", max_chars))
                )
                return ToolResult(
                    content=f"Observed target text in Chrome page:\n{excerpt}"
                )
            time.sleep(float(arguments.get("poll_interval_secs", poll_interval_secs)))
        return ToolResult(
            content=f"Timed out waiting for text in Chrome page: {arguments['wait_for']}",
            is_error=True,
        )

    return ToolResult(
        content=(
            "Unsupported chrome_browser action. Use one of: open_url, get_state, "
            "get_page_text, fill_prompt, submit_prompt, wait_for_text, "
            "wait_for_idle, run_javascript."
        ),
        is_error=True,
    )


@registry.register(
    name="screenshot",
    description=(
        "Capture a screenshot of the current desktop and optionally return OCR text. "
        "This is privacy-sensitive and requires confirmation."
    ),
    requires_confirmation=True,
)
def screenshot(output_path: str = "", ocr: bool = True) -> ToolResult:
    """Capture a screenshot of the current screen."""
    system_name = platform.system().lower()
    target_path = _resolve_capture_path(output_path)

    if system_name == "darwin":
        args = ["screencapture", "-x", str(target_path)]
    elif system_name == "linux":
        if shutil.which("gnome-screenshot"):
            args = ["gnome-screenshot", "--file", str(target_path)]
        elif shutil.which("scrot"):
            args = ["scrot", str(target_path)]
        else:
            return ToolResult(
                content="Screenshot is unsupported on this Linux system (need gnome-screenshot or scrot).",
                is_error=True,
            )
    else:
        return ToolResult(
            content=f"Screenshot is not supported on platform: {platform.system()}",
            is_error=True,
        )

    result = _run_subprocess(args)
    if result.returncode != 0:
        stderr = result.stderr.strip() or result.stdout.strip() or "unknown error"
        return ToolResult(
            content=f"Failed to capture screenshot: {stderr}", is_error=True
        )

    lines = [f"Saved screenshot to {target_path}."]
    if ocr:
        extracted_text = _extract_text_from_image(target_path)
        if extracted_text:
            lines.append("OCR text:")
            lines.append(_truncate(extracted_text, 2500))
        else:
            lines.append("OCR text was unavailable on this system.")

    return ToolResult(content="\n".join(lines))


@registry.register(
    name="desktop_control",
    description=(
        "Perform explicit local desktop actions like opening apps, typing text, "
        "sending key presses, or opening URLs. This is high risk and requires confirmation."
    ),
    requires_confirmation=True,
)
def desktop_control(
    action: str,
    application: str = "",
    text: str = "",
    key: str = "",
    modifiers: str = "",
    url: str = "",
) -> ToolResult:
    """Control local desktop actions for a narrow MVP command set."""
    system_name = platform.system().lower()
    normalized_action = action.strip().lower()

    if system_name != "darwin":
        return ToolResult(
            content=(
                "desktop_control MVP is currently implemented for macOS only. "
                f"Current platform: {platform.system()}"
            ),
            is_error=True,
        )

    try:
        if normalized_action == "open_application":
            if not application.strip():
                raise ValueError("open_application requires `application`.")
            args = ["open", "-a", application]
            summary = f"Opened application: {application}"
        elif normalized_action == "activate_application":
            if not application.strip():
                raise ValueError("activate_application requires `application`.")
            script = f'tell application "{_escape_applescript_text(application)}" to activate'
            args = ["osascript", "-e", script]
            summary = f"Activated application: {application}"
        elif normalized_action == "open_url":
            target_url = url.strip()
            if not target_url:
                raise ValueError("open_url requires `url`.")
            if not urlparse(target_url).scheme:
                target_url = "https://" + target_url
            args = ["open", target_url]
            summary = f"Opened URL: {target_url}"
        elif normalized_action == "type_text":
            if not text:
                raise ValueError("type_text requires `text`.")
            script = (
                'tell application "System Events" '
                f'to keystroke "{_escape_applescript_text(text)}"'
            )
            args = ["osascript", "-e", script]
            summary = f"Typed text ({len(text)} chars)"
        elif normalized_action == "press_key":
            if not key.strip():
                raise ValueError("press_key requires `key`.")
            script = _build_hotkey_applescript(key, "")
            args = ["osascript", "-e", script]
            summary = f"Pressed key: {key}"
        elif normalized_action == "hotkey":
            if not key.strip():
                raise ValueError("hotkey requires `key`.")
            script = _build_hotkey_applescript(key, modifiers)
            args = ["osascript", "-e", script]
            summary = f"Pressed hotkey: {modifiers or 'no modifiers'} + {key}"
        elif normalized_action == "quit_application":
            if not application.strip():
                raise ValueError("quit_application requires `application`.")
            script = (
                f'tell application "{_escape_applescript_text(application)}" to quit'
            )
            args = ["osascript", "-e", script]
            summary = f"Quit application: {application}"
        else:
            return ToolResult(
                content=(
                    "Unsupported desktop_control action. Use one of: "
                    "open_application, activate_application, open_url, type_text, "
                    "press_key, hotkey, quit_application."
                ),
                is_error=True,
            )
    except ValueError as e:
        return ToolResult(content=f"desktop_control argument error: {e}", is_error=True)

    result = _run_subprocess(args)
    if result.returncode != 0:
        stderr = result.stderr.strip() or result.stdout.strip() or "unknown error"
        return ToolResult(content=f"desktop_control failed: {stderr}", is_error=True)

    details = result.stdout.strip()
    if details:
        return ToolResult(content=f"{summary}\n{_truncate(details, 1500)}")
    return ToolResult(content=summary)


# -----------------------------------------------------------------------------
# System Tools
# -----------------------------------------------------------------------------


@registry.register(
    name="shell",
    description="Execute a shell command on the host OS. This includes running build scripts, python commands, git, etc.",
    requires_confirmation=True,  # Shell is always dangerous
)
def shell(command: str) -> ToolResult:
    """Run a shell command."""
    try:
        result = subprocess.run(
            command,
            shell=True,
            capture_output=True,
            text=True,
        )
    except Exception as e:
        return ToolResult(content=f"Shell execution failed: {e}", is_error=True)

    output = result.stdout
    if result.stderr:
        output += f"\n[stderr]:\n{result.stderr}"

    return ToolResult(
        content=f"Command executed. Exit code: {result.returncode}\nOutput:\n{_truncate(output)}",
        is_error=result.returncode != 0,
    )


# -----------------------------------------------------------------------------
# Swarm Tools (Orchestration context needed)
# -----------------------------------------------------------------------------


@registry.register(
    name="delegate_task",
    description="Ask another specialized professional agent to do a task and return the result to you. Use this to consult domain experts.",
    requires_confirmation=False,
)
async def delegate_task(role_name: str, task_description: str) -> ToolResult:
    """Spawn or consult a sub-agent for a specific task."""
    import pathlib

    from nerv.orchestrator.orchestrator import Orchestrator

    # We build a temporary orchestrator for the sub-agent
    orch = Orchestrator(pathlib.Path.cwd())

    # Fake the route result
    from nerv.models import RouteResult

    route = RouteResult(
        intent=role_name,
        complexity="high",
        model_tier=1,
        agent_type=role_name,  # This forces the Factory to spawn this exact role!
        reply="",
    )

    # We prefix the message so the sub-agent knows its context
    prompt = f"[DELEGATION TASK]\nYou have been spawned by a higher-level architect agent to handle a sub-task.\nTask Description: {task_description}"

    response = await orch.dispatch(prompt, route)

    return ToolResult(content=f"Expert ({role_name}) replied: {response}")
