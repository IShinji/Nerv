"""Tests for Tool Layer."""

from pathlib import Path

import nerv.tools.builtins as builtins
from nerv.tools.registry import registry


class _DummyResponse:
    def __init__(
        self,
        text: str,
        url: str,
        status_code: int = 200,
        content_type: str = "text/html; charset=utf-8",
    ) -> None:
        self.text = text
        self.url = url
        self.status_code = status_code
        self.headers = {"content-type": content_type}

    def raise_for_status(self) -> None:
        return None


class _DummyClient:
    def __init__(self, response: _DummyResponse) -> None:
        self._response = response

    def __enter__(self) -> "_DummyClient":
        return self

    def __exit__(self, exc_type, exc, tb) -> None:
        return None

    def get(self, *args, **kwargs) -> _DummyResponse:
        return self._response


class _DummyCompletedProcess:
    def __init__(self, returncode: int = 0, stdout: str = "", stderr: str = "") -> None:
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


def test_registry_schemas_generation() -> None:
    """Test tools are registered correctly."""
    schemas = registry.get_schemas(
        [
            "file_io",
            "list_workflows",
            "get_workflow",
            "propose_workflow",
            "list_skills",
            "get_skill",
            "list_review_queue",
            "web_search",
            "browser",
            "browser_interactive",
            "mcp_presets",
            "mcp_status",
            "chrome_browser",
            "screenshot",
            "desktop_control",
        ]
    )
    assert len(schemas) == 15

    names = [schema["function"]["name"] for schema in schemas]
    assert names == [
        "file_io",
        "list_workflows",
        "get_workflow",
        "propose_workflow",
        "list_skills",
        "get_skill",
        "list_review_queue",
        "web_search",
        "browser",
        "browser_interactive",
        "mcp_presets",
        "mcp_status",
        "chrome_browser",
        "screenshot",
        "desktop_control",
    ]
    assert "operation" in schemas[0]["function"]["parameters"]["properties"]
    assert "agent_name" in schemas[1]["function"]["parameters"]["properties"]
    assert "name" in schemas[2]["function"]["parameters"]["properties"]
    assert "steps" in schemas[3]["function"]["parameters"]["properties"]
    assert "agent_name" in schemas[4]["function"]["parameters"]["properties"]
    assert "name" in schemas[5]["function"]["parameters"]["properties"]
    assert "status" in schemas[6]["function"]["parameters"]["properties"]
    assert "query" in schemas[7]["function"]["parameters"]["properties"]
    assert "url" in schemas[8]["function"]["parameters"]["properties"]
    assert "action" in schemas[9]["function"]["parameters"]["properties"]
    assert "preset_name" in schemas[10]["function"]["parameters"]["properties"]
    assert "server_name" in schemas[11]["function"]["parameters"]["properties"]
    assert "action" in schemas[12]["function"]["parameters"]["properties"]
    assert "ocr" in schemas[13]["function"]["parameters"]["properties"]
    assert "action" in schemas[14]["function"]["parameters"]["properties"]


def test_greedy_tools() -> None:
    """Test builtin tools behavior without executing side effects too badly."""
    tool = registry.get_tool("shell")
    assert tool is not None
    assert tool.requires_confirmation is True

    tool = registry.get_tool("read_file")
    assert tool is not None
    assert tool.requires_confirmation is False

    tool = registry.get_tool("current_time")
    assert tool is not None
    assert tool.requires_confirmation is False

    tool = registry.get_tool("list_workflows")
    assert tool is not None
    assert tool.requires_confirmation is False

    tool = registry.get_tool("propose_workflow")
    assert tool is not None
    assert tool.requires_confirmation is False

    tool = registry.get_tool("web_search")
    assert tool is not None
    assert tool.requires_confirmation is False

    tool = registry.get_tool("browser")
    assert tool is not None
    assert tool.requires_confirmation is False

    tool = registry.get_tool("browser_interactive")
    assert tool is not None
    assert tool.requires_confirmation is False

    tool = registry.get_tool("mcp_presets")
    assert tool is not None
    assert tool.requires_confirmation is False

    tool = registry.get_tool("mcp_status")
    assert tool is not None
    assert tool.requires_confirmation is False

    tool = registry.get_tool("chrome_browser")
    assert tool is not None
    assert tool.requires_confirmation is False

    tool = registry.get_tool("screenshot")
    assert tool is not None
    assert tool.requires_confirmation is True

    tool = registry.get_tool("desktop_control")
    assert tool is not None
    assert tool.requires_confirmation is True


def test_current_time_supports_city_alias() -> None:
    """Test the current_time tool accepts simple city aliases."""
    tool = registry.get_tool("current_time")
    assert tool is not None

    result = tool.func(timezone="Seattle")
    assert result.is_error is False
    assert "America/Los_Angeles" in result.content


def test_file_io_can_list_and_read(tmp_path: Path) -> None:
    """Test the file_io convenience wrapper."""
    notes = tmp_path / "notes.txt"
    notes.write_text("hello world", encoding="utf-8")

    tool = registry.get_tool("file_io")
    assert tool is not None

    list_result = tool.func(operation="list", path=str(tmp_path))
    assert list_result.is_error is False
    assert "notes.txt" in list_result.content

    read_result = tool.func(operation="read", path=str(notes))
    assert read_result.is_error is False
    assert "hello world" in read_result.content


def test_web_search_parses_results(monkeypatch) -> None:
    """Test web_search extracts titles and links from search HTML."""
    html = """
    <html><body>
      <a class="result__a" href="https://example.com/article">Example Result</a>
      <div class="result__snippet">Useful snippet here.</div>
    </body></html>
    """
    response = _DummyResponse(html, "https://html.duckduckgo.com/html/?q=test")
    monkeypatch.setattr(builtins.httpx, "Client", lambda **kwargs: _DummyClient(response))

    tool = registry.get_tool("web_search")
    assert tool is not None

    result = tool.func(query="test", max_results=3)
    assert result.is_error is False
    assert "Example Result" in result.content
    assert "https://example.com/article" in result.content


def test_browser_extracts_page_content(monkeypatch) -> None:
    """Test browser fetches a page and extracts readable content."""
    html = """
    <html>
      <head><title>Example Page</title></head>
      <body>
        <p>Hello from the page.</p>
        <a href="/next">Next link</a>
      </body>
    </html>
    """
    response = _DummyResponse(html, "https://example.com/page")
    monkeypatch.setattr(builtins.httpx, "Client", lambda **kwargs: _DummyClient(response))

    tool = registry.get_tool("browser")
    assert tool is not None

    result = tool.func(url="https://example.com/page", include_links=True)
    assert result.is_error is False
    assert "Title: Example Page" in result.content
    assert "Hello from the page." in result.content
    assert "https://example.com/next" in result.content


def test_chrome_browser_open_url(monkeypatch) -> None:
    """Test chrome_browser builds the expected open_url script."""
    captured: list[list[str]] = []

    def fake_run(args: list[str]) -> _DummyCompletedProcess:
        captured.append(args)
        return _DummyCompletedProcess(stdout="Opened URL: https://gemini.google.com")

    monkeypatch.setattr(builtins.platform, "system", lambda: "Darwin")
    monkeypatch.setattr(builtins.pathlib.Path, "exists", lambda self: True)
    monkeypatch.setattr(builtins, "_run_subprocess", fake_run)

    tool = registry.get_tool("chrome_browser")
    assert tool is not None

    result = tool.func(action="open_url", url="gemini.google.com")
    assert result.is_error is False
    assert "Opened URL: https://gemini.google.com" in result.content
    assert captured
    assert captured[0][0] == "osascript"


def test_chrome_browser_fill_prompt(monkeypatch) -> None:
    """Test chrome_browser uses JS execution for prompt filling."""
    monkeypatch.setattr(
        builtins,
        "_chrome_execute_javascript",
        lambda script: builtins.ToolResult(content="filled:textarea"),
    )

    tool = registry.get_tool("chrome_browser")
    assert tool is not None

    result = tool.func(action="fill_prompt", text="hello gemini")
    assert result.is_error is False
    assert "Filled prompt" in result.content


def test_chrome_browser_wait_for_text(monkeypatch) -> None:
    """Test chrome_browser waits until target text appears."""
    responses = iter(
        [
            builtins.ToolResult(content="loading..."),
            builtins.ToolResult(content="result: finished answer"),
        ]
    )

    monkeypatch.setattr(builtins, "_chrome_page_text", lambda max_chars: next(responses))
    monkeypatch.setattr(builtins.time, "sleep", lambda _: None)

    tool = registry.get_tool("chrome_browser")
    assert tool is not None

    result = tool.func(
        action="wait_for_text",
        wait_for="finished answer",
        timeout_secs=2,
        poll_interval_secs=0.01,
    )
    assert result.is_error is False
    assert "Observed target text" in result.content
    assert "finished answer" in result.content


def test_workflow_tools_use_review_queue(monkeypatch, tmp_path: Path) -> None:
    """Test workflow tools can read the shared library and submit reviews."""
    workflows_dir = tmp_path / "workflows"
    workflows_dir.mkdir(parents=True, exist_ok=True)
    (tmp_path / "reviews" / "workflows").mkdir(parents=True, exist_ok=True)
    (tmp_path / "skills").mkdir(parents=True, exist_ok=True)

    (workflows_dir / "demo.yaml").write_text(
        """
name: "DemoWorkflow"
description: "Test workflow"
owner_agents: ["general"]
tools: ["chrome_browser"]
steps:
  - "step one"
review_status: "approved"
""".strip(),
        encoding="utf-8",
    )

    monkeypatch.setattr(builtins, "_get_project_root", lambda: tmp_path)

    list_tool = registry.get_tool("list_workflows")
    assert list_tool is not None
    list_result = list_tool.func(agent_name="general")
    assert "DemoWorkflow" in list_result.content

    get_tool = registry.get_tool("get_workflow")
    assert get_tool is not None
    get_result = get_tool.func(name="DemoWorkflow")
    assert "step one" in get_result.content

    propose_tool = registry.get_tool("propose_workflow")
    assert propose_tool is not None
    propose_result = propose_tool.func(
        name="NewFlow",
        description="A newly proposed workflow",
        owner_agents="general",
        tools="chrome_browser",
        steps="open page\nextract answer",
    )
    assert "Workflow proposal queued for review" in propose_result.content

    review_tool = registry.get_tool("list_review_queue")
    assert review_tool is not None
    review_result = review_tool.func(status="pending")
    assert "NewFlow" in review_result.content


def test_skill_tools_read_skill_packages(monkeypatch, tmp_path: Path) -> None:
    """Test skill tools load SKILL.md packages."""
    skill_dir = tmp_path / "skills" / "browser_operator"
    skill_dir.mkdir(parents=True, exist_ok=True)
    skill_dir.joinpath("SKILL.md").write_text(
        """---
name: "BrowserOperator"
description: "Operate browser tasks."
owner_agents: ["general"]
recommended_workflows: ["AskGeminiAndReturnAnswer"]
---
# Browser Operator

Do browser work carefully.
""",
        encoding="utf-8",
    )
    (tmp_path / "workflows").mkdir(parents=True, exist_ok=True)
    (tmp_path / "reviews" / "workflows").mkdir(parents=True, exist_ok=True)

    monkeypatch.setattr(builtins, "_get_project_root", lambda: tmp_path)

    list_tool = registry.get_tool("list_skills")
    assert list_tool is not None
    list_result = list_tool.func(agent_name="general")
    assert "BrowserOperator" in list_result.content

    get_tool = registry.get_tool("get_skill")
    assert get_tool is not None
    get_result = get_tool.func(name="BrowserOperator")
    assert "Do browser work carefully." in get_result.content


def test_desktop_control_open_application_on_macos(monkeypatch) -> None:
    """Test desktop_control builds the expected macOS command."""
    captured: list[list[str]] = []

    def fake_run(args: list[str]) -> _DummyCompletedProcess:
        captured.append(args)
        return _DummyCompletedProcess()

    monkeypatch.setattr(builtins.platform, "system", lambda: "Darwin")
    monkeypatch.setattr(builtins, "_run_subprocess", fake_run)

    tool = registry.get_tool("desktop_control")
    assert tool is not None

    result = tool.func(action="open_application", application="Safari")
    assert result.is_error is False
    assert "Opened application: Safari" in result.content
    assert captured == [["open", "-a", "Safari"]]


def test_screenshot_captures_to_explicit_path(monkeypatch, tmp_path: Path) -> None:
    """Test screenshot uses macOS screencapture and returns the file path."""
    captured: list[list[str]] = []
    target = tmp_path / "capture.png"

    def fake_run(args: list[str]) -> _DummyCompletedProcess:
        captured.append(args)
        target.write_text("fake image", encoding="utf-8")
        return _DummyCompletedProcess()

    monkeypatch.setattr(builtins.platform, "system", lambda: "Darwin")
    monkeypatch.setattr(builtins, "_run_subprocess", fake_run)
    monkeypatch.setattr(builtins, "_extract_text_from_image", lambda path: "Button OK")

    tool = registry.get_tool("screenshot")
    assert tool is not None

    result = tool.func(output_path=str(target), ocr=True)
    assert result.is_error is False
    assert str(target) in result.content
    assert "Button OK" in result.content
    assert captured == [["screencapture", "-x", str(target)]]
