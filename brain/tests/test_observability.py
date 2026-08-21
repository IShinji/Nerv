"""Tests for the structured observability helpers."""

import json
from pathlib import Path

import pytest

from nerv.observability import log_event, new_trace_id, timed


def test_trace_id_is_short_and_unique() -> None:
    a, b = new_trace_id(), new_trace_id()
    assert a != b
    assert len(a) == 12


def test_log_event_writes_jsonl_to_trace_file(tmp_path: Path, monkeypatch) -> None:
    trace_file = tmp_path / "trace.jsonl"
    monkeypatch.setenv("NERV_TRACE_FILE", str(trace_file))

    log_event("model_call", provider="claude-cli", cost_usd=0.02)

    line = trace_file.read_text(encoding="utf-8").strip()
    record = json.loads(line)
    assert record["event"] == "model_call"
    assert record["provider"] == "claude-cli"
    assert record["cost_usd"] == 0.02


def test_timed_logs_success_with_duration(tmp_path: Path, monkeypatch) -> None:
    trace_file = tmp_path / "trace.jsonl"
    monkeypatch.setenv("NERV_TRACE_FILE", str(trace_file))

    with timed("turn", intent="general") as extra:
        extra["agent"] = "general"

    record = json.loads(trace_file.read_text(encoding="utf-8").strip())
    assert record["event"] == "turn"
    assert record["ok"] is True
    assert record["agent"] == "general"
    assert "duration_ms" in record


def test_timed_logs_failure_and_reraises(tmp_path: Path, monkeypatch) -> None:
    trace_file = tmp_path / "trace.jsonl"
    monkeypatch.setenv("NERV_TRACE_FILE", str(trace_file))

    with pytest.raises(ValueError):
        with timed("turn"):
            raise ValueError("nope")

    record = json.loads(trace_file.read_text(encoding="utf-8").strip())
    assert record["ok"] is False
    assert "ValueError" in record["error"]
