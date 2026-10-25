"""Tests for the Memory and Context Manager modules."""

import json
from pathlib import Path

from nerv.memory.context import ContextManager, estimate_tokens
from nerv.memory.manager import DEFAULT_SCOPE, MemoryManager, scope_for
from nerv.memory.models import ChatMessage, DailyConversation
from nerv.orchestrator.registry import AgentDefinition
from nerv.orchestrator.runtime_policy import build_system_prompt


def test_memory_manager_save_and_load(tmp_path: Path) -> None:
    """Test saving and loading daily conversations."""
    manager = MemoryManager(tmp_path)

    # Write a test conversation
    conv = DailyConversation(
        date="2026-04-11",
        messages=[
            ChatMessage(role="user", content="hello"),
            ChatMessage(role="assistant", content="hi there"),
        ],
    )
    manager.save_conversation(conv)

    # Check file exists and is parsable — one JSON object per line
    file_path = manager._get_daily_file("2026-04-11")
    assert file_path.exists()

    lines = file_path.read_text(encoding="utf-8").splitlines()
    assert len(lines) == 2
    assert json.loads(lines[0])["content"] == "hello"
    assert json.loads(lines[1])["content"] == "hi there"

    # And it round-trips back through the manager
    reloaded = manager.load_conversation("2026-04-11")
    assert [m.content for m in reloaded.messages] == ["hello", "hi there"]


def test_memory_manager_load_recent(tmp_path: Path) -> None:
    """Test loading backwards across multiple days."""
    manager = MemoryManager(tmp_path)
    manager._ensure_directories()

    # Store day 1
    conv1 = DailyConversation(
        date="2026-04-10",
        messages=[
            ChatMessage(role="user", content="day 1 msg 1"),
            ChatMessage(role="assistant", content="day 1 msg 2"),
        ],
    )
    manager.save_conversation(conv1)

    # Store day 2
    conv2 = DailyConversation(
        date="2026-04-11",
        messages=[
            ChatMessage(role="user", content="day 2 msg 1"),
        ],
    )
    manager.save_conversation(conv2)

    # Load recent 2 messages - should get day 2 msg 1, day 1 msg 2
    recent = manager.load_recent_messages(limit=2)
    assert len(recent) == 2
    assert recent[0].content == "day 1 msg 2"
    assert recent[1].content == "day 2 msg 1"


def test_context_manager_build_messages_limits(tmp_path: Path) -> None:
    """Test building context bounds the messages correctly."""
    manager = MemoryManager(tmp_path)
    manager._ensure_directories()
    context = ContextManager(manager)

    # Keep the context small enough to force truncation while still fitting
    # the shared runtime policy, current user message, and one short history entry.
    agent = AgentDefinition(
        name="Test",
        description="test",
        system_prompt="SYS",
    )
    user_msg = "hello"
    short_history = "short"
    oversized_history = "huge history message that does not fit at all"
    base_tokens = estimate_tokens(build_system_prompt(agent))
    lower_bound = (
        base_tokens + estimate_tokens(user_msg) + estimate_tokens(short_history)
    ) / 0.9
    upper_bound = (
        base_tokens
        + estimate_tokens(user_msg)
        + estimate_tokens(short_history)
        + estimate_tokens(oversized_history)
    ) / 0.9
    agent.max_context_tokens = int((lower_bound + upper_bound) / 2)

    manager.append_message("user", oversized_history)
    manager.append_message("assistant", short_history)

    messages = context.build_messages(agent, user_msg)
    # Should contain System, "short" (because it fits), and "hello".
    # "huge history message..." should have been dropped.
    assert len(messages) == 3
    assert messages[0]["role"] == "system"
    assert "SYS" in messages[0]["content"]
    assert messages[1]["role"] == "assistant"
    assert messages[1]["content"] == "short"
    assert messages[2]["role"] == "user"
    assert messages[2]["content"] == "hello"


def test_context_manager_facts_injected(tmp_path: Path) -> None:
    """Test that facts are injected into system prompt."""
    manager = MemoryManager(tmp_path)
    manager._ensure_directories()
    manager.append_fact("User is a developer.")

    context = ContextManager(manager)
    agent = AgentDefinition(
        name="Test", description="test", system_prompt="You are an AI."
    )

    messages = context.build_messages(agent, "hello")
    assert "User is a developer." in messages[0]["content"]
    assert "You are an AI." in messages[0]["content"]


def test_context_manager_injects_runtime_policy(tmp_path: Path) -> None:
    """Test that shared runtime rules are injected automatically."""
    manager = MemoryManager(tmp_path)
    manager._ensure_directories()
    context = ContextManager(manager)
    agent = AgentDefinition(
        name="General",
        description="test",
        system_prompt="You are an AI.",
        tools=["current_time", "chrome_browser"],
    )

    messages = context.build_messages(agent, "西雅图时间现在几点")
    system_prompt = messages[0]["content"]

    assert "[Runtime Policy]" in system_prompt
    assert "Reply in the same language as the user's latest message" in system_prompt
    assert "[Tool Policy]" in system_prompt
    assert "Use current_time for questions about the current time" in system_prompt
    assert "prefer a configured MCP browser provider" in system_prompt


def test_context_manager_injects_extra_system_sections(tmp_path: Path) -> None:
    """Extra workflow/skill sections should be appended to the system prompt."""
    manager = MemoryManager(tmp_path)
    manager._ensure_directories()
    context = ContextManager(manager)
    agent = AgentDefinition(
        name="General",
        description="test",
        system_prompt="You are an AI.",
    )

    messages = context.build_messages(
        agent,
        "hello",
        extra_system_sections=[
            "[Workflow Library]\n- Demo",
            "[Skill Library]\n- Skill",
        ],
    )

    system_prompt = messages[0]["content"]
    assert "[Workflow Library]" in system_prompt
    assert "[Skill Library]" in system_prompt


def _append_worker(root: str, tag: str) -> None:
    """Top-level so it is importable by a spawned process."""
    manager = MemoryManager(Path(root), "concurrent")
    for i in range(20):
        manager.append_message("user", f"{tag}-{i}")


def test_scope_for_derives_stable_slug() -> None:
    """Channel + sender collapse into one filesystem-safe scope."""
    assert scope_for() == DEFAULT_SCOPE
    assert scope_for("", "") == DEFAULT_SCOPE
    assert scope_for("telegram", "12345") == "telegram-12345"
    assert scope_for("Telegram", "user@example.com") == "telegram-user-example.com"
    # Path separators must never survive into a directory name.
    assert "/" not in scope_for("cli", "../../etc/passwd")


def test_conversations_are_isolated_per_scope(tmp_path: Path) -> None:
    """Two senders on the same instance must not read each other's history."""
    alice = MemoryManager(tmp_path, scope_for("telegram", "alice"))
    bob = MemoryManager(tmp_path, scope_for("telegram", "bob"))

    alice.append_message("user", "my bank pin is 1234")
    bob.append_message("user", "hello")

    assert [m.content for m in alice.load_recent_messages()] == ["my bank pin is 1234"]
    assert [m.content for m in bob.load_recent_messages()] == ["hello"]
    assert alice.conv_dir != bob.conv_dir


def test_default_scope_keeps_legacy_paths(tmp_path: Path) -> None:
    """An existing install's conversations stay where they were."""
    manager = MemoryManager(tmp_path)
    assert manager.conv_dir == tmp_path / "personal" / "memory" / "conversations"


def test_facts_are_shared_across_scopes(tmp_path: Path) -> None:
    """Facts describe the owner, so they follow them across channels."""
    cli = MemoryManager(tmp_path)
    telegram = MemoryManager(tmp_path, scope_for("telegram", "owner"))

    cli.append_fact("Owner prefers metric units.")
    assert "metric units" in telegram.read_facts()


def test_legacy_json_day_is_read_and_migrated(tmp_path: Path) -> None:
    """Pre-JSONL day files still load, and fold in on the next append."""
    manager = MemoryManager(tmp_path)
    today = manager._get_today_str()
    legacy = manager._legacy_daily_file(today)
    legacy.write_text(
        DailyConversation(
            date=today,
            messages=[ChatMessage(role="user", content="old turn")],
        ).model_dump_json(),
        encoding="utf-8",
    )

    # Readable before any migration happens.
    assert [m.content for m in manager.load_recent_messages()] == ["old turn"]

    manager.append_message("assistant", "new turn")

    assert not legacy.exists()
    assert manager._get_daily_file(today).exists()
    assert [m.content for m in manager.load_recent_messages()] == [
        "old turn",
        "new turn",
    ]


def test_append_is_durable_under_concurrent_processes(tmp_path: Path) -> None:
    """Concurrent appends from separate processes must not lose messages."""
    import multiprocessing

    ctx = multiprocessing.get_context("spawn")
    procs = [
        ctx.Process(target=_append_worker, args=(str(tmp_path), tag))
        for tag in ("a", "b", "c")
    ]
    for proc in procs:
        proc.start()
    for proc in procs:
        proc.join(timeout=60)
        assert proc.exitcode == 0

    manager = MemoryManager(tmp_path, "concurrent")
    contents = {m.content for m in manager.load_recent_messages(limit=1000)}
    assert len(contents) == 60


def test_estimate_tokens_is_close_to_real_ratios() -> None:
    """Latin text is ~4 chars/token; CJK is ~1 token/char."""
    latin = "the quick brown fox jumps over the lazy dog"  # 43 chars
    assert 10 <= estimate_tokens(latin) <= 16

    cjk = "西雅图时间现在几点"  # 9 chars
    assert estimate_tokens(cjk) == 9

    assert estimate_tokens("") == 0
