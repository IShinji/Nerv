"""Tests for sectioned long-term facts and on-demand loading."""

from pathlib import Path

from nerv.memory.context import FACTS_INLINE_TOKEN_BUDGET, ContextManager
from nerv.memory.manager import MemoryManager
from nerv.orchestrator.registry import AgentDefinition

_FACTS = """\
# Work
Ships the Nerv gateway.
Prefers Rust for the core.

# Travel
Based in Seattle.
Flies out of SEA.
"""


def _agent() -> AgentDefinition:
    return AgentDefinition(
        name="tester",
        description="d",
        system_prompt="p",
        model_tier=1,
        max_context_tokens=8000,
    )


def test_fact_sections_split_on_headings(tmp_path: Path) -> None:
    manager = MemoryManager(tmp_path)
    manager.facts_file.write_text(_FACTS, encoding="utf-8")

    assert [head for head, _ in manager.fact_sections()] == ["Work", "Travel"]
    assert "Prefers Rust" in manager.read_fact_section("Work")
    # Heading lookup is case-insensitive.
    assert manager.read_fact_section("travel").startswith("Based in Seattle")
    assert manager.read_fact_section("nope") == ""


def test_unstructured_facts_round_trip_as_preamble(tmp_path: Path) -> None:
    """A legacy headingless facts file is not silently dropped."""
    manager = MemoryManager(tmp_path)
    manager.facts_file.write_text(
        "- likes tea\n- dislikes meetings\n", encoding="utf-8"
    )

    sections = manager.fact_sections()
    assert len(sections) == 1
    assert sections[0][0] == ""
    assert "likes tea" in sections[0][1]


def test_outline_lists_titles_with_a_hint(tmp_path: Path) -> None:
    manager = MemoryManager(tmp_path)
    manager.facts_file.write_text(_FACTS, encoding="utf-8")

    outline = manager.facts_outline()
    assert "- Work: Ships the Nerv gateway." in outline
    assert "- Travel: Based in Seattle." in outline
    # Each entry is a title plus a one-line hint, not the whole section.
    assert "Prefers Rust" not in outline
    assert "Flies out of SEA" not in outline


def test_small_facts_are_inlined(tmp_path: Path) -> None:
    manager = MemoryManager(tmp_path)
    manager.facts_file.write_text(_FACTS, encoding="utf-8")

    messages = ContextManager(manager).build_messages(_agent(), "hi")
    system = messages[0]["content"]
    assert "[Long-term Facts]" in system
    assert "Prefers Rust" in system


def test_large_facts_collapse_to_an_outline(tmp_path: Path) -> None:
    """Past the budget the prompt carries titles, not the whole file."""
    manager = MemoryManager(tmp_path)
    filler = "x " * (FACTS_INLINE_TOKEN_BUDGET * 4)
    manager.facts_file.write_text(
        f"# Work\n{filler}\n\n# Travel\nBased in Seattle.\n", encoding="utf-8"
    )

    system = ContextManager(manager).build_messages(_agent(), "hi")[0]["content"]
    assert "[Long-term Facts — outline]" in system
    assert "get_fact_section" in system
    assert "- Work:" in system and "- Travel:" in system
    assert filler.strip() not in system


def test_large_unstructured_facts_are_still_inlined(tmp_path: Path) -> None:
    """With no headings there is nothing to outline, so facts are not lost."""
    manager = MemoryManager(tmp_path)
    body = "important detail " * (FACTS_INLINE_TOKEN_BUDGET * 2)
    manager.facts_file.write_text(body, encoding="utf-8")

    system = ContextManager(manager).build_messages(_agent(), "hi")[0]["content"]
    assert "[Long-term Facts]" in system
    assert "important detail" in system


def test_get_fact_section_tool(tmp_path: Path, monkeypatch) -> None:
    from nerv.tools import memory as memory_tools

    monkeypatch.setattr(memory_tools, "_get_project_root", lambda: tmp_path)
    MemoryManager(tmp_path).facts_file.write_text(_FACTS, encoding="utf-8")

    hit = memory_tools.get_fact_section("Work")
    assert not hit.is_error and "Prefers Rust" in hit.content

    listing = memory_tools.get_fact_section()
    assert not listing.is_error and "Travel" in listing.content

    miss = memory_tools.get_fact_section("Hobbies")
    assert miss.is_error and "Work" in miss.content
