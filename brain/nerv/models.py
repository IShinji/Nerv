"""Pydantic data models for the brain layer."""

from pydantic import BaseModel, Field


class RouteResult(BaseModel):
    """Result of intent classification by the Router."""

    intent: str = Field(description="Classified intent category")
    complexity: str = Field(default="low", description="Task complexity: low, medium, high")
    model_tier: int = Field(default=0, description="Suggested model tier (0-3)")
    agent_type: str = Field(default="general", description="Suggested agent type")
    needs_capabilities: list[str] = Field(
        default_factory=list,
        description="Abstract capabilities likely needed for the task",
    )
    preferred_execution_mode: str = Field(
        default="auto",
        description="Execution preference such as auto, mcp_preferred, or native_only",
    )
    reply: str = Field(
        default="",
        description="Direct reply text, if the router can answer without agent dispatch",
    )
