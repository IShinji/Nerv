"""Tests for the Memory and Context Manager modules."""

import json
from pathlib import Path

from nerv.memory.context import ContextManager
from nerv.memory.manager import MemoryManager
from nerv.memory.models import ChatMessage, DailyConversation
from nerv.orchestrator.registry import AgentDefinition


def test_memory_manager_save_and_load(tmp_path: Path) -> None:
    """Test saving and loading daily conversations."""
    manager = MemoryManager(tmp_path)
    
    # Write a test conversation
    conv = DailyConversation(date="2026-04-11", messages=[
        ChatMessage(role="user", content="hello"),
        ChatMessage(role="assistant", content="hi there")
    ])
    manager.save_conversation(conv)
    
    # Check file exists and is parsable
    file_path = manager._get_daily_file("2026-04-11")
    assert file_path.exists()
    
    with open(file_path, "r", encoding="utf-8") as f:
        data = json.load(f)
        assert len(data["messages"]) == 2
        
def test_memory_manager_load_recent(tmp_path: Path) -> None:
    """Test loading backwards across multiple days."""
    manager = MemoryManager(tmp_path)
    manager._ensure_directories()
    
    # Store day 1
    conv1 = DailyConversation(date="2026-04-10", messages=[
        ChatMessage(role="user", content="day 1 msg 1"),
        ChatMessage(role="assistant", content="day 1 msg 2"),
    ])
    manager.save_conversation(conv1)
    
    # Store day 2
    conv2 = DailyConversation(date="2026-04-11", messages=[
        ChatMessage(role="user", content="day 2 msg 1"),
    ])
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
    
    # Create an agent with very small max context (just enough for sys + user + 1 history)
    agent = AgentDefinition(
        name="Test",
        description="test",
        system_prompt="SYS",
        max_context_tokens=30  # Very small
    )
    # Tokens used: SYS(3).
    # Current msg: "hello"(5).
    # Available for history: 30 - 3 - 5 - 3(buffer) = 19
    
    manager.append_message("user", "huge history message that does not fit at all") # 45 chars
    manager.append_message("assistant", "short") # 5 chars
    
    messages = context.build_messages(agent, "hello")
    # Should contain System, "short" (because it fits), and "hello". 
    # "huge history message..." should have been dropped.
    assert len(messages) == 3
    assert messages[0]["role"] == "system"
    assert messages[0]["content"] == "SYS"
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
    agent = AgentDefinition(name="Test", description="test", system_prompt="You are an AI.")
    
    messages = context.build_messages(agent, "hello")
    assert "User is a developer." in messages[0]["content"]
    assert "You are an AI." in messages[0]["content"]
