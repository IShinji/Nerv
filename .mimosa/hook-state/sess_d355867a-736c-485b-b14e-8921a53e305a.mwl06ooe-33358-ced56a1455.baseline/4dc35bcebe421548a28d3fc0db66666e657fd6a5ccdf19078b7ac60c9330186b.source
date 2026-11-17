"""Model availability for the workflow fallback must follow the provider prefix."""

from pathlib import Path

import pytest

from nerv.orchestrator.orchestrator import Orchestrator


@pytest.fixture
def orch(tmp_path: Path) -> Orchestrator:
    return Orchestrator(tmp_path)


@pytest.mark.parametrize(
    "model, env_var",
    [
        ("litellm:gemini/gemini-2.0-flash", "GEMINI_API_KEY"),
        ("litellm:claude-opus-4-8", "ANTHROPIC_API_KEY"),
        ("litellm:gpt-5", "OPENAI_API_KEY"),
        ("claude-opus-4-8", "ANTHROPIC_API_KEY"),  # bare name == litellm
        ("litellm:deepseek/deepseek-chat", "DEEPSEEK_API_KEY"),
    ],
)
async def test_litellm_available_when_key_present(
    orch: Orchestrator, monkeypatch: pytest.MonkeyPatch, model: str, env_var: str
) -> None:
    monkeypatch.setenv(env_var, "test-key")
    assert await orch._model_available(model) is True


async def test_litellm_unavailable_without_key(
    orch: Orchestrator, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("GEMINI_API_KEY", raising=False)
    assert await orch._model_available("litellm:gemini/gemini-2.0-flash") is False
