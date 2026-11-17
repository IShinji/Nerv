"""Tests for the unified LLM client dispatch (the provider switch point)."""

import pytest

from nerv.llm import client as llm_client
from nerv.llm.client import CompletionResult, complete, parse_model


def test_parse_model_recognizes_provider_prefixes() -> None:
    assert parse_model("claude-cli:opus") == ("claude-cli", "opus")
    assert parse_model("local:qwen2.5:1.5b") == ("local", "qwen2.5:1.5b")
    assert parse_model("litellm:claude-opus-4-8") == ("litellm", "claude-opus-4-8")
    # Bare cloud names route through litellm for the reserved API path.
    assert parse_model("claude-opus-4-8") == ("litellm", "claude-opus-4-8")


@pytest.mark.asyncio
async def test_claude_cli_prefix_dispatches_to_cli_backend(monkeypatch) -> None:
    captured = {}

    async def fake_cli(messages, *, model_name, **kwargs):
        captured["model_name"] = model_name
        return CompletionResult(content="cli reply", runs_own_tool_loop=True)

    monkeypatch.setattr("nerv.llm.claude_cli.complete_claude_cli", fake_cli)

    result = await complete(
        [{"role": "user", "content": "hi"}], model="claude-cli:opus"
    )

    assert result.content == "cli reply"
    assert result.runs_own_tool_loop is True
    assert captured["model_name"] == "opus"


@pytest.mark.asyncio
async def test_local_prefix_still_routes_to_ollama(monkeypatch) -> None:
    """Reserve guarantee: a `local:` model must hit the Ollama backend."""
    captured = {}

    async def fake_ollama(messages, *, model_name, **kwargs):
        captured["model_name"] = model_name
        return CompletionResult(content="ollama reply")

    monkeypatch.setattr(llm_client, "_complete_ollama", fake_ollama)

    result = await complete(
        [{"role": "user", "content": "hi"}], model="local:qwen2.5:1.5b"
    )

    assert result.content == "ollama reply"
    assert result.runs_own_tool_loop is False
    assert captured["model_name"] == "qwen2.5:1.5b"
