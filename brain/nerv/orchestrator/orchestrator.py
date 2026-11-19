"""Orchestrator — receives routing decisions and dispatches to agents.

This is the central coordinator that:
1. Receives a RouteResult from the Router
2. Finds the matching agent in the registry
3. Calls the appropriate model with the agent's system prompt
4. Returns the response
"""

import asyncio
import inspect
import json
import logging
import os
import re
from dataclasses import asdict
from pathlib import Path
from typing import Any

import httpx

# Registration side effect: the capability registry pulls in the builtin
# tools (including evolution/productivity) on import.
from nerv.capabilities import capability_registry  # noqa: F401
from nerv.config import get_tier_models, get_workspace_root
from nerv.memory.manager import scope_for, use_scope
from nerv.orchestrator.registry import AgentRegistry
from nerv.security import (
    PendingActionStore,
    PendingRecord,
    confirmation_reason,
    executing_confirmed,
)
from nerv.workflows.registry import ReviewQueue, WorkflowRegistry

logger = logging.getLogger(__name__)

DEFAULT_OLLAMA_URL = "http://localhost:11434"
OLLAMA_TIMEOUT = 60.0

CONFIRM_COMMANDS = {"confirm", "approve", "run", "yes", "y", "ok"}
CANCEL_COMMANDS = {"cancel", "deny", "reject", "no", "n", "stop"}
CASUAL_WORDS = {"yes", "y", "ok", "no", "n", "stop", "run"}


class Orchestrator:
    """Dispatch tasks to agents based on routing decisions."""

    def __init__(self, project_root: Path) -> None:
        self._project_root = project_root
        self.registry = AgentRegistry(project_root)
        self.workflow_registry = WorkflowRegistry(project_root)
        self.review_queue = ReviewQueue(project_root)
        self.base_url = os.environ.get("NERV_OLLAMA_URL", DEFAULT_OLLAMA_URL)
        self._client = httpx.AsyncClient(timeout=OLLAMA_TIMEOUT)
        # Strong refs to fire-and-forget tasks; asyncio only holds weak ones,
        # so without this the GC can cancel a summary mid-flight.
        self._background_tasks: set[asyncio.Task[None]] = set()
        # File-backed so actions queued by the MCP server (a separate process)
        # are visible to this orchestrator's confirm/cancel flow.
        self._pending_store = PendingActionStore(project_root)
        self._workspace_root = get_workspace_root(project_root)
        logger.info(
            "Orchestrator initialized with %d agents", len(self.registry.list_agents())
        )

    async def dispatch_pending_command(
        self, message: str, channel: str = "", sender: str = ""
    ) -> str | None:
        """Handle ``confirm <id>`` / ``cancel <id>`` without any routing.

        Kept for python-side callers (the workflow executor's approval flow);
        the turn path handles pending commands in Rust.
        """
        scope = scope_for(channel, sender)
        with use_scope(scope):
            reply = await self._handle_pending_action_command(
                message, sender=sender, channel=channel
            )
            return reply or None

    async def execute_pending_record(
        self,
        action_id: int,
        tool_name: str = "",
        arguments: dict[str, Any] | None = None,
        justification: str = "",
    ) -> dict[str, Any]:
        """Execute one confirmed action for the Rust pending flow (RPC wrapper).

        Python-only tools confirmed by the user run here; the record is
        rebuilt from the RPC payload so no store round trip is needed.
        """
        record = PendingRecord(
            id=action_id,
            tool_name=tool_name,
            arguments=arguments or {},
            justification=justification,
        )
        content = await self._execute_pending_action(record)
        return {"content": content}

    async def run_tool(
        self,
        name: str,
        arguments: dict[str, Any],
        sender: str = "",
        channel: str = "",
        scope: str = "",
    ) -> dict[str, Any]:
        """One tool call for the Rust loop: resolve, gate, execute or suspend.

        Mirrors the per-tool-call block of the old `_call_model`.
        """

        resolution = capability_registry.resolve(name)
        if not resolution:
            # Agent hallucinated a tool
            return {"content": f"Unknown tool: {name}", "suspended": False}

        tool_def = resolution.tool_def
        executable_name = resolution.provider_name

        reason = confirmation_reason(
            executable_name,
            arguments,
            self._workspace_root,
            requires_confirmation=tool_def.requires_confirmation,
            project_root=self._project_root,
        )
        if reason:
            pending_action = self._queue_pending_action(
                executable_name,
                arguments,
                reason=reason,
                sender=sender,
                channel=channel,
            )
            notification = (
                "⚠️ [SYSTEM BACKGROUND ALERT] "
                f"Pending action #{pending_action.id} ({executable_name}) is "
                "waiting for confirmation."
            )
            logger.warning(
                "Tool %s suspended for confirmation: %s", executable_name, reason
            )
            return {
                "content": (
                    "[ACTION SUSPENDED] " + self._format_pending_action(pending_action)
                ),
                "suspended": True,
                "pending": asdict(pending_action),
                "notification": notification,
            }

        logger.debug("Executing tool %s with args %s", executable_name, arguments)
        if inspect.iscoroutinefunction(tool_def.func):
            tool_result = await tool_def.func(**arguments)
        else:
            tool_result = tool_def.func(**arguments)
        return {"content": tool_result.content, "suspended": False}

    async def finish_turn(
        self,
        reply: str,
        channel: str = "",
        sender: str = "",
        known_pending: list[dict[str, Any]] | None = None,
        notifications: list[str] | None = None,
    ) -> dict[str, Any]:
        """Deprecated wrapper: finalization now runs in Rust. Kept only for
        in-process callers until the sidecar retires."""
        known_ids = {int(item["id"]) for item in (known_pending or []) if "id" in item}
        final_reply, pending_created = self._append_new_pending(
            reply, known_ids, sender, channel
        )
        return {"reply": final_reply, "pending_created": pending_created}

    async def complete_turn(
        self,
        messages: list[dict[str, Any]],
        model: str,
        tools: list[dict[str, Any]] | None = None,
        scope: str = "",
    ) -> dict[str, Any]:
        """One python llm call for backends the rust client doesn't serve.

        Only the litellm path routes here (a python-only dependency); the
        claude-cli and local providers run in Rust. Python loads the user's
        personal API keys before calling cloud providers.
        """
        with use_scope(scope or "default"):
            env_path = self._project_root / "personal" / "api_keys.env"
            if env_path.exists():
                from dotenv import load_dotenv

                load_dotenv(env_path)

            from nerv.llm import complete

            result = await complete(
                messages,
                model=model,
                tools=tools,
                project_root=self._project_root,
            )
        return {
            "content": result.content,
            "tool_calls": result.tool_calls,
            "runs_own_tool_loop": result.runs_own_tool_loop,
            "provider": result.provider,
            "duration_ms": result.duration_ms,
            "cost_usd": result.cost_usd,
            "usage": result.usage,
        }

    def _append_new_pending(
        self, response: str, known_ids: set[int], sender: str, channel: str
    ) -> tuple[str, bool]:
        """Mention actions queued during the turn that the reply doesn't cover.

        Under the claude-cli backend the MCP server (a separate process) queues
        suspended actions in the shared store; the model may not mention them,
        so the user would never learn that something awaits approval. Returns
        the (possibly extended) reply and whether anything new was surfaced.
        """
        new_actions = [
            a
            for a in self._visible_pending_actions(sender, channel)
            if a.id not in known_ids and f"#{a.id}" not in response
        ]
        if not new_actions:
            return response, False
        lines = ["", "Waiting for your confirmation:"]
        for action in new_actions:
            reason = f" — {action.reason}" if action.reason else ""
            lines.append(f"- {self._format_pending_action(action)}{reason}")
        lines.append('Reply "confirm <id>" to run one, or "cancel <id>" to discard it.')
        return response + "\n".join(lines), True

    def _select_model(self, tier: int) -> str:
        """Select a provider-prefixed model string for a tier (from config).

        Shared with the python WorkflowExecutor; the turn path selects models
        in Rust.
        """
        tier_models = get_tier_models(self._project_root)
        model = tier_models.get(tier, tier_models[0])
        return model

    async def _model_available(self, model: str) -> bool:
        """Whether the provider-prefixed ``model`` can serve a request right now."""
        import shutil

        from nerv.llm.client import parse_model

        provider, name = parse_model(model)
        if provider == "claude-cli":
            # Uses the logged-in subscription, not an API key.
            from nerv.llm.claude_cli import CLAUDE_BIN

            return shutil.which(CLAUDE_BIN) is not None
        if provider == "local":
            try:
                resp = await self._client.post(
                    f"{self.base_url}/api/show", json={"name": name}, timeout=5
                )
                return resp.status_code == 200
            except Exception:
                return False

        # litellm: the API key depends on the vendor, which is either a
        # "vendor/model" prefix or recognisable from the bare model name.
        vendor = name.split("/", 1)[0].lower()
        key_checks = {
            "gemini": "GEMINI_API_KEY",
            "claude": "ANTHROPIC_API_KEY",
            "anthropic": "ANTHROPIC_API_KEY",
            "gpt": "OPENAI_API_KEY",
            "openai": "OPENAI_API_KEY",
            "o1": "OPENAI_API_KEY",
            "deepseek": "DEEPSEEK_API_KEY",
        }
        for prefix, env_var in key_checks.items():
            if vendor.startswith(prefix):
                return bool(os.environ.get(env_var))
        return False

    def _queue_pending_action(
        self,
        tool_name: str,
        arguments: dict[str, Any],
        reason: str = "",
        sender: str = "",
        channel: str = "",
    ) -> PendingRecord:
        """Store a suspended action for later confirmation."""
        return self._pending_store.add(
            tool_name,
            arguments,
            reason=reason,
            sender=sender,
            channel=channel,
            scope=scope_for(channel, sender) if (sender or channel) else "",
        )

    def _format_pending_action(self, action: PendingRecord) -> str:
        """Render a user-facing summary of a pending action."""
        if action.tool_name == "desktop_control":
            verb = action.arguments.get("action", "desktop action")
            details = []
            for key in ("application", "url", "text", "key", "modifiers"):
                value = str(action.arguments.get(key, "")).strip()
                if value:
                    details.append(f"{key}={value}")
            detail_text = f" ({', '.join(details)})" if details else ""
            summary = (
                f"Pending action #{action.id}: desktop_control -> {verb}{detail_text}"
            )
        elif action.tool_name == "screenshot":
            output_path = (
                str(action.arguments.get("output_path", "")).strip() or "(auto path)"
            )
            summary = (
                f"Pending action #{action.id}: screenshot -> save to {output_path}"
            )
        elif action.tool_name == "execute_workflow":
            name = str(action.arguments.get("workflow_name", "")).strip() or "?"
            summary = f"Pending action #{action.id}: run workflow '{name}'"
        else:
            summary = (
                f"Pending action #{action.id}: {action.tool_name} "
                f"with arguments {json.dumps(action.arguments, ensure_ascii=False)}"
            )
        if action.justification:
            summary += f"\n  Justification: {action.justification}"
        return summary

    def _visible_pending_actions(
        self, sender: str, channel: str
    ) -> list[PendingRecord]:
        """Return pending actions that belong to exactly this sender/channel.

        An action is visible only to the scope that queued it. Unscoped actions
        (no sender, channel, or scope) are visible only to an unscoped caller
        such as the local CLI, never to a remote sender.
        """
        actions = self._pending_store.list()
        if not (sender or channel):
            return [a for a in actions if not (a.sender or a.channel or a.scope)]
        wanted = scope_for(channel, sender)
        return [
            a
            for a in actions
            if (a.scope and a.scope == wanted)
            or (
                not a.scope
                and (a.sender or a.channel)
                and a.sender == sender
                and a.channel == channel
            )
        ]

    async def _handle_pending_action_command(
        self,
        message: str,
        sender: str,
        channel: str,
    ) -> str | None:
        """Execute or cancel a queued action when the user replies with confirm/cancel."""
        visible_actions = self._visible_pending_actions(sender, channel)
        if not visible_actions:
            return None

        normalized = " ".join(message.strip().lower().split())
        if not normalized:
            return None

        command_match = re.match(
            r"^(confirm|approve|run|yes|y|ok|cancel|deny|reject|no|n|stop)(?:\s+#?(\d+))?$",
            normalized,
        )
        if not command_match:
            return None

        command = command_match.group(1)
        action_id = command_match.group(2)

        # Plain chat words ("yes", "ok", "no", "stop") must never run or cancel
        # a queued action, and running one always names its id.
        if command in CONFIRM_COMMANDS and not action_id:
            return None
        if command in CANCEL_COMMANDS and not action_id and command in CASUAL_WORDS:
            return None

        if action_id:
            target = next(
                (action for action in visible_actions if action.id == int(action_id)),
                None,
            )
            if target is None:
                return f"I could not find pending action #{action_id}."
        elif len(visible_actions) == 1:
            target = visible_actions[0]
        else:
            ids = ", ".join(str(action.id) for action in visible_actions)
            return f"Multiple pending actions are waiting: {ids}. Reply with confirm <id> or cancel <id>."

        if command in CANCEL_COMMANDS:
            self._pending_store.remove(target.id)
            return f"Cancelled pending action #{target.id}: {self._format_pending_action(target)}"

        if command in CONFIRM_COMMANDS:
            return await self._execute_pending_action(target)

        return None

    async def _execute_pending_action(self, action: PendingRecord) -> str:
        """Run a previously approved pending action."""
        from nerv.tools.builtins import registry

        tool_def = registry.get_tool(action.tool_name)
        self._pending_store.remove(action.id)

        if not tool_def:
            return (
                f"Pending action #{action.id} could not be executed because tool "
                f"{action.tool_name} is unavailable."
            )

        # Run it as the action the user confirmed, so a tool that was approved as a
        # whole (a workflow) can recognise that this is the call that was approved.
        with executing_confirmed(action):
            if inspect.iscoroutinefunction(tool_def.func):
                result = await tool_def.func(**action.arguments)
            else:
                result = await asyncio.to_thread(tool_def.func, **action.arguments)

        status = "Executed" if not result.is_error else "Execution failed for"
        return (
            f"{status} pending action #{action.id}: {self._format_pending_action(action)}\n"
            f"{result.content}"
        )

    def _handle_review_queue_command(self, message: str) -> str | None:
        """Handle direct review-queue commands before routing to an agent."""
        normalized = " ".join(message.strip().split())
        if not normalized:
            return None

        lower = normalized.lower()
        if lower in {
            "show workflow reviews",
            "list workflow reviews",
            "show review queue",
        }:
            items = self.review_queue.list_reviews(status="pending")
            if not items:
                return "Workflow review queue is empty."
            lines = ["Pending workflow reviews:"]
            for item in items:
                lines.append(
                    f"- {item.review_id}: {item.workflow.name} — {item.workflow.description}"
                )
            lines.append(
                'Reply with "approve workflow <review_id>" or "reject workflow <review_id>".'
            )
            return "\n".join(lines)

        approve_match = re.match(r"^approve workflow ([a-z0-9_-]+)$", lower)
        if approve_match:
            review_id = approve_match.group(1)
            try:
                item = self.review_queue.approve_workflow(review_id)
            except ValueError as e:
                return str(e)
            self.workflow_registry.reload()
            return (
                f"Approved workflow review {item.review_id}. "
                f"Published shared workflow: {item.workflow.name}."
            )

        reject_match = re.match(r"^reject workflow ([a-z0-9_-]+)(?:\s+(.+))?$", lower)
        if reject_match:
            review_id = reject_match.group(1)
            notes = normalized[len("reject workflow ") + len(review_id) :].strip()
            try:
                item = self.review_queue.reject_workflow(
                    review_id, reviewer_notes=notes
                )
            except ValueError as e:
                return str(e)
            return f"Rejected workflow review {item.review_id}."

        return None

    async def aclose(self) -> None:
        """Release the orchestrator's network resources.

        Called on brain shutdown so the shared httpx client's connection pool
        is closed instead of being reclaimed by the interpreter exiting.
        """
        if self._background_tasks:
            # Let in-flight summaries finish rather than losing the work; they
            # already swallow their own errors, so gather cannot raise here.
            await asyncio.gather(*self._background_tasks, return_exceptions=True)
        await self._client.aclose()
