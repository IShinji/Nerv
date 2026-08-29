"""Tests for keyword search over conversation memory and its scoping."""

from pathlib import Path

from nerv.memory.manager import DEFAULT_SCOPE, MemoryManager, active_scope, use_scope
from nerv.memory.models import ChatMessage, DailyConversation


def _day(manager: MemoryManager, date: str, *pairs: tuple[str, str]) -> None:
    manager.save_conversation(
        DailyConversation(
            date=date,
            messages=[ChatMessage(role=r, content=c) for r, c in pairs],
        )
    )


def test_search_finds_message_outside_recent_window(tmp_path: Path) -> None:
    """A hit far older than the history window is still retrievable."""
    manager = MemoryManager(tmp_path)
    _day(manager, "2026-01-05", ("user", "the telegram adapter drops long replies"))
    for day in range(10, 25):
        _day(manager, f"2026-02-{day}", ("user", "unrelated chatter"))

    # It is well past the 10-message recency window the context manager uses.
    recent = manager.load_recent_messages(limit=10)
    assert all("telegram" not in m.content for m in recent)

    hits = manager.search_messages("telegram adapter")
    assert len(hits) == 1
    assert hits[0].date == "2026-01-05"


def test_search_ranks_whole_phrase_above_scattered_terms(tmp_path: Path) -> None:
    manager = MemoryManager(tmp_path)
    _day(manager, "2026-03-01", ("user", "a rate limit on the upload path"))
    _day(manager, "2026-03-02", ("user", "the rate is fine but the limit is not"))

    hits = manager.search_messages("rate limit")
    assert hits[0].date == "2026-03-01"
    assert hits[0].score > hits[1].score


def test_search_recency_breaks_score_ties(tmp_path: Path) -> None:
    manager = MemoryManager(tmp_path)
    _day(manager, "2026-03-01", ("user", "deploy the gateway"))
    _day(manager, "2026-03-09", ("user", "deploy the gateway"))

    hits = manager.search_messages("deploy the gateway")
    assert [h.date for h in hits] == ["2026-03-09", "2026-03-01"]


def test_search_filters_by_date_and_role(tmp_path: Path) -> None:
    manager = MemoryManager(tmp_path)
    _day(manager, "2026-03-01", ("user", "ship it"), ("assistant", "ship it"))
    _day(manager, "2026-03-05", ("user", "ship it"))

    assert len(manager.search_messages("ship it")) == 3
    assert len(manager.search_messages("ship it", since="2026-03-02")) == 1
    assert len(manager.search_messages("ship it", until="2026-03-01")) == 2
    roles = manager.search_messages("ship it", role="assistant")
    assert len(roles) == 1 and roles[0].message.role == "assistant"


def test_search_honours_limit_and_empty_query(tmp_path: Path) -> None:
    manager = MemoryManager(tmp_path)
    _day(manager, "2026-03-01", *[("user", "repeat")] * 8)

    assert len(manager.search_messages("repeat", limit=3)) == 3
    assert manager.search_messages("   ") == []


def test_search_never_crosses_scopes(tmp_path: Path) -> None:
    """One sender's search must not reach another sender's history."""
    alice = MemoryManager(tmp_path, scope_for_test := "telegram-alice")
    bob = MemoryManager(tmp_path, "telegram-bob")
    _day(alice, "2026-03-01", ("user", "my bank password hint"))
    _day(bob, "2026-03-01", ("user", "bob talks about lunch"))

    assert scope_for_test == "telegram-alice"
    assert bob.search_messages("bank password") == []
    assert len(alice.search_messages("bank password")) == 1


def test_active_scope_defaults_closed_and_binds(tmp_path: Path) -> None:
    """An unbound scope is DEFAULT_SCOPE, never a wildcard."""
    assert active_scope() == DEFAULT_SCOPE
    with use_scope("telegram-alice"):
        assert active_scope() == "telegram-alice"
    assert active_scope() == DEFAULT_SCOPE

    # An empty scope must not become something permissive either.
    with use_scope(""):
        assert active_scope() == DEFAULT_SCOPE


# ── The search_memory tool ───────────────────────────────────────────────────


def test_search_memory_tool_reports_hits(tmp_path: Path, monkeypatch) -> None:
    from nerv.tools import memory as memory_tools

    monkeypatch.setattr(memory_tools, "_get_project_root", lambda: tmp_path)
    manager = MemoryManager(tmp_path)
    _day(manager, "2026-03-01", ("user", "the vite upgrade broke the peer deps"))

    result = memory_tools.search_memory("vite upgrade")
    assert not result.is_error
    assert "2026-03-01" in result.content
    assert "peer deps" in result.content


def test_search_memory_tool_rejects_empty_query(tmp_path: Path, monkeypatch) -> None:
    from nerv.tools import memory as memory_tools

    monkeypatch.setattr(memory_tools, "_get_project_root", lambda: tmp_path)
    result = memory_tools.search_memory("  ")
    assert result.is_error


def test_search_memory_tool_reports_no_matches(tmp_path: Path, monkeypatch) -> None:
    from nerv.tools import memory as memory_tools

    monkeypatch.setattr(memory_tools, "_get_project_root", lambda: tmp_path)
    MemoryManager(tmp_path)  # create the directories
    result = memory_tools.search_memory("nothing here")
    assert not result.is_error
    assert "No past messages" in result.content


def test_search_memory_tool_stays_in_the_active_scope(
    tmp_path: Path, monkeypatch
) -> None:
    """The tool reads the bound scope, so it cannot see another sender."""
    from nerv.tools import memory as memory_tools

    monkeypatch.setattr(memory_tools, "_get_project_root", lambda: tmp_path)
    _day(MemoryManager(tmp_path, "telegram-alice"), "2026-03-01", ("user", "alice key"))

    with use_scope("telegram-bob"):
        assert "No past messages" in memory_tools.search_memory("alice key").content
    with use_scope("telegram-alice"):
        assert "alice key" in memory_tools.search_memory("alice key").content
