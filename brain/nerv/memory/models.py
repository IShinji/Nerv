"""Data models for Chat Memory."""
from datetime import datetime
from typing import Any

from pydantic import BaseModel, Field


class ChatMessage(BaseModel):
    """A single chat message in the conversation history."""

    role: str  # "user", "assistant", "system", "tool"
    content: str
    timestamp: datetime = Field(default_factory=datetime.now)
    metadata: dict[str, Any] = Field(default_factory=dict)


class DailyConversation(BaseModel):
    """Daily conversation storage model."""

    date: str  # YYYY-MM-DD format
    messages: list[ChatMessage] = Field(default_factory=list)
    summary: str = ""  # AI generated summary for the day if it gets too long
