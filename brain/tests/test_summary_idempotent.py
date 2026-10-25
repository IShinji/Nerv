"""Rolling-summary backfill must not summarise the same day twice."""

import asyncio
from pathlib import Path
from types import SimpleNamespace

from nerv.memory import summarizer
from nerv.memory.manager import MemoryManager
from nerv.memory.models import ChatMessage, DailyConversation


def _day(manager: MemoryManager, date: str, content: str) -> None:
    manager.save_conversation(
        DailyConversation(
            date=date, messages=[ChatMessage(role="user", content=content)]
        )
    )


async def test_concurrent_backfills_summarise_each_day_once(
    tmp_path: Path, monkeypatch
) -> None:
    manager = MemoryManager(tmp_path)
    _day(manager, "2020-03-01", "talk one")
    _day(manager, "2020-03-02", "talk two")

    calls = 0

    async def fake_complete(messages, **kwargs):
        nonlocal calls
        calls += 1
        await asyncio.sleep(0.05)  # keep the first run in flight
        return SimpleNamespace(content="a summary")

    monkeypatch.setattr("nerv.llm.complete", fake_complete)

    await asyncio.gather(
        summarizer.backfill_summaries(manager, model="x"),
        summarizer.backfill_summaries(manager, model="x"),
        summarizer.backfill_summaries(manager, model="x"),
    )
    assert calls == 2  # one per day, not one per run per day
    # A later run finds nothing left to do.
    assert await summarizer.backfill_summaries(manager, model="x") == 0
    assert calls == 2
