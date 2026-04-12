"""Workflow registry and review queue."""

from __future__ import annotations

import datetime as dt
import re
from pathlib import Path
from typing import TYPE_CHECKING

import yaml

from nerv.workflows.models import WorkflowDefinition, WorkflowReviewItem

if TYPE_CHECKING:
    from nerv.orchestrator.registry import AgentDefinition


def _slugify(value: str) -> str:
    """Convert a label into a filesystem-safe slug."""
    normalized = re.sub(r"[^a-z0-9]+", "_", value.strip().lower())
    normalized = re.sub(r"_+", "_", normalized).strip("_")
    return normalized or "workflow"


def workflow_to_dict(workflow: WorkflowDefinition) -> dict[str, object]:
    """Serialize a workflow for YAML persistence."""
    return {
        "name": workflow.name,
        "description": workflow.description,
        "tags": workflow.tags,
        "owner_agents": workflow.owner_agents,
        "tools": workflow.tools,
        "steps": workflow.steps,
        "success_criteria": workflow.success_criteria,
        "created_by": workflow.created_by,
        "review_status": workflow.review_status,
    }


class WorkflowRegistry:
    """Load approved workflows from the shared workflow library."""

    def __init__(self, project_root: Path) -> None:
        self.project_root = project_root
        self.workflows_dir = project_root / "workflows"
        self._workflows: dict[str, WorkflowDefinition] = {}
        self.reload()

    def reload(self) -> None:
        """Reload workflows from disk."""
        self._workflows = {}
        if not self.workflows_dir.exists():
            return

        for path in sorted(self.workflows_dir.rglob("*.yaml")):
            workflow = self._parse_workflow_file(path)
            if workflow.review_status != "approved":
                continue
            self._workflows[workflow.name.lower()] = workflow

    def _parse_workflow_file(self, path: Path) -> WorkflowDefinition:
        """Parse a workflow definition YAML file."""
        with open(path, encoding="utf-8") as f:
            data = yaml.safe_load(f) or {}

        return WorkflowDefinition(
            name=data["name"],
            description=data.get("description", ""),
            tags=data.get("tags", []),
            owner_agents=data.get("owner_agents", []),
            tools=data.get("tools", []),
            steps=data.get("steps", []),
            success_criteria=data.get("success_criteria", []),
            created_by=data.get("created_by", "preset"),
            review_status=data.get("review_status", "approved"),
            source_path=str(path),
        )

    def list_workflows(self) -> list[WorkflowDefinition]:
        """Return all approved workflows."""
        return list(self._workflows.values())

    def find(self, name: str) -> WorkflowDefinition | None:
        """Find a workflow by name."""
        return self._workflows.get(name.strip().lower())

    def find_for_agent(self, agent: "AgentDefinition") -> list[WorkflowDefinition]:
        """Return workflows relevant to a specific agent."""
        agent_name = agent.name.strip().lower()
        agent_tags = {tag.lower() for tag in agent.tags}

        matches: list[WorkflowDefinition] = []
        for workflow in self._workflows.values():
            owners = {owner.lower() for owner in workflow.owner_agents}
            tags = {tag.lower() for tag in workflow.tags}
            if agent_name in owners or agent_tags.intersection(tags):
                matches.append(workflow)

        return matches

    def render_catalog(self, agent: "AgentDefinition", max_items: int = 4) -> str:
        """Render a compact workflow catalog for prompt injection."""
        workflows = self.find_for_agent(agent)
        if not workflows:
            return ""

        lines = ["[Workflow Library]"]
        for workflow in workflows[:max_items]:
            tools = ", ".join(workflow.tools) if workflow.tools else "none"
            lines.append(f"- {workflow.name}: {workflow.description}")
            lines.append(f"  Tools: {tools}")
            if workflow.steps:
                lines.append(f"  Steps: {' -> '.join(workflow.steps[:4])}")
        lines.append(
            "- Prefer an existing shared workflow when it clearly matches the task. "
            "If no shared workflow fits, you may draft a new one and send it to the review queue."
        )
        return "\n".join(lines)


class ReviewQueue:
    """File-backed review queue for workflow proposals."""

    def __init__(self, project_root: Path) -> None:
        self.project_root = project_root
        self.reviews_dir = project_root / "reviews" / "workflows"
        self.workflows_dir = project_root / "workflows"
        self.reviews_dir.mkdir(parents=True, exist_ok=True)
        self.workflows_dir.mkdir(parents=True, exist_ok=True)

    def list_reviews(self, status: str = "") -> list[WorkflowReviewItem]:
        """List workflow review items, optionally filtered by status."""
        items: list[WorkflowReviewItem] = []
        for path in sorted(self.reviews_dir.glob("*.yaml")):
            item = self._parse_review_file(path)
            if status and item.status != status:
                continue
            items.append(item)
        return items

    def get(self, review_id: str) -> WorkflowReviewItem | None:
        """Get a review item by ID."""
        path = self.reviews_dir / f"{review_id}.yaml"
        if not path.exists():
            return None
        return self._parse_review_file(path)

    def submit_workflow(
        self,
        workflow: WorkflowDefinition,
        proposed_by: str = "agent",
    ) -> WorkflowReviewItem:
        """Create a pending review item for a workflow proposal."""
        review_id = self._new_review_id(workflow.name)
        workflow.review_status = "pending"

        item = WorkflowReviewItem(
            review_id=review_id,
            status="pending",
            proposed_by=proposed_by,
            created_at=dt.datetime.now(dt.UTC).replace(microsecond=0).isoformat(),
            workflow=workflow,
        )
        self._write_review(item)
        return item

    def approve_workflow(self, review_id: str) -> WorkflowReviewItem:
        """Approve a pending workflow and publish it to the shared library."""
        item = self.get(review_id)
        if item is None:
            raise ValueError(f"Unknown workflow review: {review_id}")

        item.status = "approved"
        item.workflow.review_status = "approved"

        target_path = self.workflows_dir / f"{_slugify(item.workflow.name)}.yaml"
        with open(target_path, "w", encoding="utf-8") as f:
            yaml.safe_dump(
                workflow_to_dict(item.workflow),
                f,
                allow_unicode=True,
                sort_keys=False,
            )

        self._write_review(item)
        return item

    def reject_workflow(self, review_id: str, reviewer_notes: str = "") -> WorkflowReviewItem:
        """Reject a pending workflow proposal."""
        item = self.get(review_id)
        if item is None:
            raise ValueError(f"Unknown workflow review: {review_id}")

        item.status = "rejected"
        item.reviewer_notes = reviewer_notes
        self._write_review(item)
        return item

    def _new_review_id(self, name: str) -> str:
        """Generate a stable, readable review ID."""
        timestamp = dt.datetime.now(dt.UTC).strftime("%Y%m%d%H%M%S")
        return f"wf-{timestamp}-{_slugify(name)}"

    def _parse_review_file(self, path: Path) -> WorkflowReviewItem:
        """Parse a workflow review YAML file."""
        with open(path, encoding="utf-8") as f:
            data = yaml.safe_load(f) or {}

        workflow_data = data.get("workflow", {})
        workflow = WorkflowDefinition(
            name=workflow_data["name"],
            description=workflow_data.get("description", ""),
            tags=workflow_data.get("tags", []),
            owner_agents=workflow_data.get("owner_agents", []),
            tools=workflow_data.get("tools", []),
            steps=workflow_data.get("steps", []),
            success_criteria=workflow_data.get("success_criteria", []),
            created_by=workflow_data.get("created_by", "agent"),
            review_status=workflow_data.get("review_status", data.get("status", "pending")),
            source_path=workflow_data.get("source_path", ""),
        )
        return WorkflowReviewItem(
            review_id=data["review_id"],
            status=data.get("status", "pending"),
            proposed_by=data.get("proposed_by", "agent"),
            created_at=data.get("created_at", ""),
            workflow=workflow,
            reviewer_notes=data.get("reviewer_notes", ""),
            source_path=str(path),
        )

    def _write_review(self, item: WorkflowReviewItem) -> None:
        """Persist a review item back to disk."""
        path = self.reviews_dir / f"{item.review_id}.yaml"
        payload = {
            "review_id": item.review_id,
            "status": item.status,
            "proposed_by": item.proposed_by,
            "created_at": item.created_at,
            "reviewer_notes": item.reviewer_notes,
            "workflow": workflow_to_dict(item.workflow),
        }
        with open(path, "w", encoding="utf-8") as f:
            yaml.safe_dump(payload, f, allow_unicode=True, sort_keys=False)
