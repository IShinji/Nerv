"""Desktop and browser automation: Chrome control, screenshots, input events.

These are the outward-acting tools: `desktop_control` and any path outside the
workspace sandbox are suspended for user confirmation before they run — see
`nerv.security.sandbox`.

The AppleScript and subprocess primitives live in this module rather than in
`_helpers` because nothing else uses them, and because the tools below call
them through this module's own namespace.
"""

import datetime as dt
import json
import logging
import pathlib
import platform
import shutil
import subprocess
import tempfile
import time
from urllib.parse import urlparse

from nerv.tools._helpers import (
    _ensure_url,
    _get_project_root,
    _resolve_path,
    _truncate,
)
from nerv.tools.registry import ToolResult, registry

logger = logging.getLogger(__name__)

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
