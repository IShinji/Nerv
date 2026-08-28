"""Manager for long-term and short-term memory persistence.

Handles writing conversations to personal/memory/conversations/YYYY-MM-DD.json
and facts to personal/memory/facts.md.
"""

import json
import logging
from datetime import datetime
from pathlib import Path

from nerv.memory.models import ChatMessage, DailyConversation

logger = logging.getLogger(__name__)


class MemoryManager:
    """Manages persistence of conversation and facts."""

    def __init__(self, project_root: Path) -> None:
        self.memory_dir = project_root / "personal" / "memory"
        self.conv_dir = self.memory_dir / "conversations"
        self.facts_file = self.memory_dir / "facts.md"
        self._ensure_directories()

    def _ensure_directories(self) -> None:
        """Create necessary memory directories if they don't exist."""
        self.conv_dir.mkdir(parents=True, exist_ok=True)
        if not self.facts_file.exists():
            self.facts_file.parent.mkdir(parents=True, exist_ok=True)
            self.facts_file.touch()

    def _get_today_str(self) -> str:
        """Get current date as YYYY-MM-DD."""
        return datetime.now().strftime("%Y-%m-%d")

    def _get_daily_file(self, date_str: str) -> Path:
        """Get the file path for a specific day."""
        return self.conv_dir / f"{date_str}.json"

    def load_today_conversation(self) -> DailyConversation:
        """Load today's conversation from disk, or create a new one."""
        today = self._get_today_str()
        file_path = self._get_daily_file(today)

        if not file_path.exists():
            return DailyConversation(date=today, messages=[])

        try:
            with open(file_path, encoding="utf-8") as f:
                data = json.load(f)
                return DailyConversation.model_validate(data)
        except Exception as e:
            logger.error("Failed to load conversation from %s: %s", file_path, e)
            return DailyConversation(date=today, messages=[])

    def save_conversation(self, conv: DailyConversation) -> None:
        """Save a daily conversation to disk."""
        file_path = self._get_daily_file(conv.date)
        try:
            with open(file_path, "w", encoding="utf-8") as f:
                json.dump(conv.model_dump(mode="json"), f, ensure_ascii=False, indent=2)
                f.write("\n")
        except Exception as e:
            logger.error("Failed to save conversation to %s: %s", file_path, e)

    def load_recent_messages(self, limit: int = 20) -> list[ChatMessage]:
        """Load the most recent messages across days up to the limit."""
        # Find all json files in chronological order
        files = sorted(self.conv_dir.glob("*.json"))
        if not files:
            return []

        messages: list[ChatMessage] = []
        # Walk backwards starting from the newest file
        for file_path in reversed(files):
            try:
                with open(file_path, encoding="utf-8") as f:
                    data = json.load(f)
                    conv = DailyConversation.model_validate(data)
                    # Add messages from this file (newest first for now, we'll reverse later)
                    for msg in reversed(conv.messages):
                        messages.append(msg)
                        if len(messages) >= limit:
                            break
                if len(messages) >= limit:
                    break
            except Exception as e:
                logger.error(
                    "Failed to load historical conversation %s: %s", file_path, e
                )

        # Reverse back so the oldest is first, newest is last
        return list(reversed(messages))

    def append_message(self, role: str, content: str) -> None:
        """Append a single message to today's conversation and save it."""
        conv = self.load_today_conversation()
        msg = ChatMessage(role=role, content=content)
        conv.messages.append(msg)
        self.save_conversation(conv)

    def read_facts(self) -> str:
        """Read long-term facts."""
        if not self.facts_file.exists():
            return ""
        return self.facts_file.read_text(encoding="utf-8")

    def append_fact(self, fact: str) -> None:
        """Append a new long-term fact."""
        content = self.read_facts()
        if content and not content.endswith("\n"):
            content += "\n"
        content += f"- {fact}\n"
        self.facts_file.write_text(content, encoding="utf-8")
