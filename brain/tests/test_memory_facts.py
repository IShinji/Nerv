"""Tests for sectioned long-term facts and on-demand loading."""

from pathlib import Path

from nerv.memory.manager import MemoryManager

_FACTS = """\
# Work
Ships the Nerv gateway.
Prefers Rust for the core.

# Travel
Based in Seattle.
Flies out of SEA.
"""


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
