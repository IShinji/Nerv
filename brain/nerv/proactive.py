"""Proactive background monitor — the source of Nerv reaching out first.

Runs on the gateway heartbeat. It is deliberately *deterministic and LLM-free*
so heartbeats stay free (the whole token-efficiency premise): it parses local
state (calendar, unfinished workflow runs) and emits reminders, deduplicated via
a small state file so the same thing is never surfaced twice.
"""

from __future__ import annotations

import datetime as dt
import json
import logging
from pathlib import Path

from nerv.config import load_project_config
from nerv.tasks import TaskRunStore

logger = logging.getLogger(__name__)

DEFAULT_REMINDER_MINUTES = 60


class ProactiveMonitor:
    """Scan local state and surface fresh, de-duplicated reminders."""

    def __init__(self, project_root: Path) -> None:
        self._root = project_root
        self._calendar_path = project_root / "personal" / "calendar.ics"
        self._state_path = project_root / "personal" / "proactive_state.json"
        self._task_store = TaskRunStore(project_root)
        cfg = load_project_config(project_root).get("proactive", {})
        cfg = cfg if isinstance(cfg, dict) else {}
        self.enabled = bool(cfg.get("enabled", True))
        self.reminder_minutes = int(
            cfg.get("calendar_reminder_minutes", DEFAULT_REMINDER_MINUTES)
        )

    def check(self, now: dt.datetime | None = None) -> list[str]:
        """Return newly-triggered reminders and persist what was surfaced."""
        if not self.enabled:
            return []
        now = now or dt.datetime.now()
        notified = self._load_notified()
        reminders: list[str] = []

        for key, message in self._calendar_due(now):
            if key not in notified:
                reminders.append(message)
                notified.add(key)

        for key, message in self._unfinished_workflows():
            if key not in notified:
                reminders.append(message)
                notified.add(key)

        if reminders:
            self._save_notified(notified)
        return reminders

    def _calendar_due(self, now: dt.datetime) -> list[tuple[str, str]]:
        """Find events starting within the reminder window."""
        if not self._calendar_path.exists():
            return []

        window_end = now + dt.timedelta(minutes=self.reminder_minutes)
        due: list[tuple[str, str]] = []
        uid = summary = start = ""
        for line in self._calendar_path.read_text(encoding="utf-8").splitlines():
            if line.startswith("UID:"):
                uid = line[len("UID:"):]
            elif line.startswith("SUMMARY:"):
                summary = line[len("SUMMARY:"):]
            elif line.startswith("DTSTART:"):
                start = line[len("DTSTART:"):]
            elif line.startswith("END:VEVENT"):
                start_dt = self._parse_ics_dt(start)
                if summary and start_dt and now <= start_dt <= window_end:
                    key = f"cal:{uid or summary + start}"
                    when = start_dt.strftime("%H:%M")
                    due.append((key, f"⏰ Reminder: '{summary}' at {when}."))
                uid = summary = start = ""
        return due

    def _unfinished_workflows(self) -> list[tuple[str, str]]:
        """Surface workflow runs that were interrupted and can be resumed."""
        out: list[tuple[str, str]] = []
        for run in self._task_store.list_incomplete():
            step = run.first_unfinished()
            step_idx = step.idx if step else "?"
            out.append(
                (
                    f"wf:{run.id}",
                    f"🔄 Unfinished workflow '{run.workflow_name}' (run {run.id}) "
                    f"is paused at step {step_idx}.",
                )
            )
        return out

    @staticmethod
    def _parse_ics_dt(value: str) -> dt.datetime | None:
        try:
            return dt.datetime.strptime(value.strip(), "%Y%m%dT%H%M%S")
        except ValueError:
            return None

    def _load_notified(self) -> set[str]:
        if not self._state_path.exists():
            return set()
        try:
            data = json.loads(self._state_path.read_text(encoding="utf-8"))
            return set(data.get("notified", []))
        except (json.JSONDecodeError, OSError):
            return set()

    def _save_notified(self, notified: set[str]) -> None:
        self._state_path.parent.mkdir(parents=True, exist_ok=True)
        # Cap stored keys so the file can't grow without bound.
        trimmed = list(notified)[-500:]
        self._state_path.write_text(
            json.dumps({"notified": trimmed}, ensure_ascii=False),
            encoding="utf-8",
        )
