"""Tests for rolling day summaries: generation, storage, and injection."""

from dataclasses import dataclass
from datetime import datetime, timedelta
from pathlib import Path

import pytest

from nerv.memory.context import SUMMARY_TOKEN_BUDGET, ContextManager
from nerv.memory.manager import MemoryManager
from nerv.memory.models import ChatMessage, DailyConversation
from nerv.orchestrator.registry import AgentDefinition


@dataclass
class _Result:
    content: str
    tool_calls: list = None  # type: ignore[assignment]


def _agent() -> AgentDefinition:
    return AgentDefinition(
        name="tester",
        description="d",
        system_prompt="p",
        model_tier=1,
        max_context_tokens=8000,
    )


def _day(manager: MemoryManager, date: str, *contents: str) -> None:
    manager.save_conversation(
        DailyConversation(
            date=date,
            messages=[ChatMessage(role="user", content=c) for c in contents],
        )
    )


def _yesterday() -> str:
    return (datetime.now() - timedelta(days=1)).strftime("%Y-%m-%d")


def test_summary_round_trips(tmp_path: Path) -> None:
    manager = MemoryManager(tmp_path)
    manager.write_summary("2026-03-01", "  Discussed the gateway rewrite.  ")
    assert manager.read_summary("2026-03-01") == "Discussed the gateway rewrite."
    assert manager.read_summary("2026-03-02") == ""


def test_days_needing_summary_excludes_today_and_summarised(tmp_path: Path) -> None:
    manager = MemoryManager(tmp_path)
    today = datetime.now().strftime("%Y-%m-%d")
    _day(manager, today, "still talking")
    _day(manager, "2026-03-01", "old talk")
    _day(manager, "2026-03-02", "other old talk")
    manager.write_summary("2026-03-02", "already done")

    pending = manager.days_needing_summary()
    assert pending == ["2026-03-01"]
    assert today not in pending


@pytest.mark.asyncio
async def test_summarize_day_stores_model_output(tmp_path: Path, monkeypatch) -> None:
    from nerv.memory import summarizer

    manager = MemoryManager(tmp_path)
    _day(manager, "2026-03-01", "we picked vite 8", "and dropped esbuild")

    seen: dict = {}

    async def fake_complete(messages, **kwargs):
        seen["prompt"] = messages[0]["content"]
        seen["model"] = kwargs["model"]
        return _Result(content="Chose vite 8 and dropped esbuild.")

    monkeypatch.setattr("nerv.llm.complete", fake_complete)

    out = await summarizer.summarize_day(manager, "2026-03-01", model="local:test")
    assert out == "Chose vite 8 and dropped esbuild."
    assert manager.read_summary("2026-03-01") == out
    # The day's turns reached the prompt.
    assert "we picked vite 8" in seen["prompt"]
    assert seen["model"] == "local:test"


@pytest.mark.asyncio
async def test_summarize_day_survives_model_failure(
    tmp_path: Path, monkeypatch
) -> None:
    """A failed summary must not raise into the turn that triggered it."""
    from nerv.memory import summarizer

    manager = MemoryManager(tmp_path)
    _day(manager, "2026-03-01", "something")

    async def boom(messages, **kwargs):
        raise RuntimeError("model down")

    monkeypatch.setattr("nerv.llm.complete", boom)

    assert await summarizer.summarize_day(manager, "2026-03-01", model="x") == ""
    assert manager.read_summary("2026-03-01") == ""


@pytest.mark.asyncio
async def test_backfill_is_bounded_per_call(tmp_path: Path, monkeypatch) -> None:
    from nerv.memory import summarizer

    manager = MemoryManager(tmp_path)
    for day in range(1, 6):
        _day(manager, f"2026-03-0{day}", "talk")

    calls = 0

    async def fake_complete(messages, **kwargs):
        nonlocal calls
        calls += 1
        return _Result(content="a summary")

    monkeypatch.setattr("nerv.llm.complete", fake_complete)

    assert await summarizer.backfill_summaries(manager, model="x", limit=2) == 2
    assert calls == 2
    # The rest stay pending for later turns.
    assert len(manager.days_needing_summary(limit=10)) == 3


def test_long_day_keeps_the_end(tmp_path: Path) -> None:
    """Truncation drops opening chatter, not the conclusions."""
    from nerv.memory import summarizer

    manager = MemoryManager(tmp_path)
    _day(manager, "2026-03-01", "x" * summarizer.MAX_CHARS_PER_DAY, "the decision")

    body = summarizer._render_day(manager, "2026-03-01")
    assert "the decision" in body
    assert body.startswith("...[earlier turns omitted]")


def test_summaries_reach_the_prompt(tmp_path: Path) -> None:
    """The gap this closes: a day outside the recency window is still visible."""
    manager = MemoryManager(tmp_path)
    _day(manager, _yesterday(), "the telegram adapter drops long replies")
    manager.write_summary(_yesterday(), "Found the telegram adapter truncation bug.")

    system = ContextManager(manager).build_messages(_agent(), "hi")[0]["content"]
    assert "[Earlier days — summaries]" in system
    assert "telegram adapter truncation" in system


def test_summary_injection_respects_its_budget(tmp_path: Path) -> None:
    manager = MemoryManager(tmp_path)
    for day in range(1, 4):
        date = (datetime.now() - timedelta(days=day)).strftime("%Y-%m-%d")
        _day(manager, date, "talk")
        manager.write_summary(date, "word " * (SUMMARY_TOKEN_BUDGET * 2))

    system = ContextManager(manager).build_messages(_agent(), "hi")[0]["content"]
    # One oversized summary alone blows the budget, so at most one gets in.
    assert system.count("- 20") <= 1


def test_no_summaries_adds_no_section(tmp_path: Path) -> None:
    manager = MemoryManager(tmp_path)
    _day(manager, "2026-03-01", "talk")

    system = ContextManager(manager).build_messages(_agent(), "hi")[0]["content"]
    assert "Earlier days" not in system
