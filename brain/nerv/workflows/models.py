"""Data models for Nerv workflows and review items."""

from dataclasses import dataclass, field
from typing import Any


@dataclass
class WorkflowStep:
    """A structured workflow step that executes one tool call."""

    id: str
    tool: str
    arguments: dict[str, Any] = field(default_factory=dict)
    description: str = ""


@dataclass
class WorkflowDefinition:
    """Reusable workflow definition stored on disk."""

    name: str
    description: str
    tags: list[str] = field(default_factory=list)
    owner_agents: list[str] = field(default_factory=list)
    tools: list[str] = field(default_factory=list)
    steps: list[WorkflowStep] = field(default_factory=list)
    success_criteria: list[str] = field(default_factory=list)
    created_by: str = "preset"
    review_status: str = "approved"
    source_path: str = ""


@dataclass
class WorkflowReviewItem:
    """A workflow proposal waiting for user approval."""

    review_id: str
    status: str
    proposed_by: str
    created_at: str
    workflow: WorkflowDefinition
    reviewer_notes: str = ""
    source_path: str = ""
