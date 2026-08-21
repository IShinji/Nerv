"""Tests for the proactive monitor (deterministic, dedup, calendar reminders)."""

import datetime as dt
from pathlib import Path

from nerv.proactive import ProactiveMonitor
from nerv.tasks import TaskRunStore


def _write_event(root: Path, uid: str, summary: str, start: dt.datetime) -> None:
    cal = root / "personal" / "calendar.ics"
    cal.parent.mkdir(parents=True, exist_ok=True)
    block = (
        "BEGIN:VEVENT\n"
        f"UID:{uid}\n"
        f"DTSTART:{start.strftime('%Y%m%dT%H%M%S')}\n"
        f"SUMMARY:{summary}\n"
        "END:VEVENT\n"
    )
    if cal.exists():
        cal.write_text(cal.read_text() + block, encoding="utf-8")
    else:
        cal.write_text(
            "BEGIN:VCALENDAR\n" + block + "END:VCALENDAR\n", encoding="utf-8"
        )


def test_reminds_about_event_in_window_once(tmp_path: Path) -> None:
    now = dt.datetime(2026, 6, 1, 9, 0, 0)
    _write_event(tmp_path, "e1", "Standup", now + dt.timedelta(minutes=15))

    monitor = ProactiveMonitor(tmp_path)
    first = monitor.check(now=now)
    assert any("Standup" in m for m in first)

    # Dedup: the same event is not surfaced again on the next heartbeat.
    second = monitor.check(now=now)
    assert second == []


def test_ignores_events_outside_window(tmp_path: Path) -> None:
    now = dt.datetime(2026, 6, 1, 9, 0, 0)
    _write_event(tmp_path, "far", "LaterMeeting", now + dt.timedelta(hours=5))
    _write_event(tmp_path, "past", "OldMeeting", now - dt.timedelta(hours=1))

    monitor = ProactiveMonitor(tmp_path)
    assert monitor.check(now=now) == []


def test_disabled_monitor_is_silent(tmp_path: Path) -> None:
    (tmp_path / "config.yaml").write_text(
        "proactive:\n  enabled: false\n", encoding="utf-8"
    )
    now = dt.datetime(2026, 6, 1, 9, 0, 0)
    _write_event(tmp_path, "e1", "Standup", now + dt.timedelta(minutes=10))

    monitor = ProactiveMonitor(tmp_path)
    assert monitor.check(now=now) == []


def test_surfaces_unfinished_workflow(tmp_path: Path) -> None:
    store = TaskRunStore(tmp_path)
    run = store.create("nightly", "x", ["step one", "step two"])

    monitor = ProactiveMonitor(tmp_path)
    messages = monitor.check()
    assert any(run.id in m and "Unfinished" in m for m in messages)
