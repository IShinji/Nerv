"""Tests for calendar, notes, and (MCP-first) email tools."""

from pathlib import Path

import pytest

from nerv.capabilities import capability_registry
from nerv.tools import productivity


@pytest.fixture(autouse=True)
def _isolated_root(tmp_path: Path, monkeypatch):
    monkeypatch.setattr(productivity, "_get_project_root", lambda: tmp_path)
    return tmp_path


def test_calendar_add_then_list() -> None:
    productivity.calendar_add("Dentist", "2026-06-10T09:00", "2026-06-10T09:30")
    productivity.calendar_add("Standup", "2026-06-09T10:00")

    listed = productivity.calendar_list()
    assert "Dentist" in listed.content
    assert "Standup" in listed.content
    # Events are sorted by start time (the 9th comes before the 10th).
    assert listed.content.index("Standup") < listed.content.index("Dentist")


def test_notes_write_read_list() -> None:
    productivity.note_write("Shopping List", "milk, eggs")
    assert "Shopping List" in productivity.note_list().content

    read = productivity.note_read("Shopping List")
    assert "milk, eggs" in read.content

    missing = productivity.note_read("does-not-exist")
    assert missing.is_error


def test_capabilities_resolve_to_productivity_tools() -> None:
    assert capability_registry.resolve("calendar.add").provider_name == "calendar_add"
    assert capability_registry.resolve("notes.write").provider_name == "note_write"
    assert capability_registry.resolve("email.send").provider_name == "send_email"


@pytest.mark.asyncio
async def test_send_email_without_provider_degrades_gracefully() -> None:
    result = await productivity.send_email("a@b.com", "Hi", "Body")
    assert result.is_error
    assert "No email provider is configured" in result.content
