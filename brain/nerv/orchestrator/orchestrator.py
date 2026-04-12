"""Orchestrator — receives routing decisions and dispatches to agents.

This is the central coordinator that:
1. Receives a RouteResult from the Router
2. Finds the matching agent in the registry
3. Calls the appropriate model with the agent's system prompt
4. Returns the response
"""

import inspect
import json
import logging
import os
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import httpx

from nerv.memory.context import ContextManager
from nerv.memory.manager import MemoryManager
from nerv.models import RouteResult
from nerv.orchestrator.registry import AgentDefinition, AgentRegistry
from nerv.skills.registry import SkillRegistry
from nerv.workflows.registry import ReviewQueue, WorkflowRegistry

logger = logging.getLogger(__name__)

DEFAULT_OLLAMA_URL = "http://localhost:11434"
OLLAMA_TIMEOUT = 60.0

CONFIRM_COMMANDS = {"confirm", "approve", "run", "yes", "y", "ok"}
CANCEL_COMMANDS = {"cancel", "deny", "reject", "no", "n", "stop"}


@dataclass
class PendingAction:
    """A suspended high-risk tool invocation awaiting user approval."""

    id: int
    tool_name: str
    arguments: dict[str, Any]
    sender: str = ""
    channel: str = ""


class Orchestrator:
    """Dispatch tasks to agents based on routing decisions."""

    def __init__(self, project_root: Path) -> None:
        self.registry = AgentRegistry(project_root)
        self.memory = MemoryManager(project_root)
        self.context_manager = ContextManager(self.memory)
        self.workflow_registry = WorkflowRegistry(project_root)
        self.skill_registry = SkillRegistry(project_root)
        self.review_queue = ReviewQueue(project_root)
        self.base_url = os.environ.get("NERV_OLLAMA_URL", DEFAULT_OLLAMA_URL)
        self._client = httpx.AsyncClient(timeout=OLLAMA_TIMEOUT)
        self._pending_notifications: list[str] = []
        self._pending_actions: list[PendingAction] = []
        self._next_pending_action_id = 1
        logger.info("Orchestrator initialized with %d agents", len(self.registry.list_agents()))

    async def dispatch(
        self,
        message: str,
        route_result: RouteResult,
        channel: str = "",
        sender: str = "",
    ) -> str:
        """Dispatch a message to the appropriate agent and return the response."""
        pending_response = await self._handle_pending_action_command(
            message,
            sender=sender,
            channel=channel,
        )
        if pending_response:
            self._remember_turn(message, pending_response)
            return pending_response

        review_response = self._handle_review_queue_command(message)
        if review_response:
            self._remember_turn(message, review_response)
            return review_response

        # If the router already provided a direct reply, use it
        if route_result.reply:
            self._remember_turn(message, route_result.reply)
            return route_result.reply

        # Find the matching agent, or manufacture one
        agent = await self._find_agent(route_result, message)
        logger.info(
            "Dispatching to agent '%s' (intent=%s, tier=%d)",
            agent.name,
            route_result.intent,
            route_result.model_tier,
        )

        # 1. Save user's message to persistent memory
        self.memory.append_message("user", message)

        # Select model based on tier
        model = self._select_model(route_result.model_tier)

        # 2. Build contextual messages (System + Facts + History + Current msg)
        # Note: the current user_msg is passed in separately for context bounds
        extra_sections = [
            self.workflow_registry.render_catalog(agent),
            self.skill_registry.render_catalog(agent),
        ]
        messages = self.context_manager.build_messages(
            agent,
            message,
            max_history_msg=10,
            extra_system_sections=extra_sections,
        )

        # Build tools payload if agent has any
        tools_schemas = None
        if agent.tools:
            from nerv.tools.builtins import registry
            tools_schemas = registry.get_schemas(agent.tools)

        # Call the model
        try:
            response = await self._call_model(
                model,
                messages,
                tools_schemas,
                sender=sender,
                channel=channel,
            )
            
            # 3. Save assistant's reply to memory
            self.memory.append_message("assistant", response)

            return response
        except Exception as e:
            logger.error("Model call failed: %s", e)
            return f"I encountered an error while processing your request: {e}"

    async def _find_agent(self, route_result: RouteResult, message_hint: str) -> AgentDefinition:
        """Find the best matching agent for the routing result, or create one if missing."""
        agent = self.registry.find_by_type(route_result.agent_type)
        if agent:
            return agent

        logger.info("Agent '%s' not found. Manufacturing via AgentFactory...", route_result.agent_type)
        
        # Avoid circular import at top level
        from nerv.orchestrator.factory import AgentFactory
        import pathlib
        
        factory = AgentFactory(pathlib.Path.cwd())
        # The user's original message is a hint for persona creation
        new_agent = await factory.create_agent(route_result.agent_type, message_hint)
        
        if new_agent:
            # Hot-reload the new agent into memory
            self.registry.agents[new_agent.name] = new_agent
            logger.info("Successfully birthed and registered new expert: '%s'", new_agent.name)
            return new_agent

        logger.warning(
            "AgentFactory failed for type '%s', using default general fallback",
            route_result.agent_type,
        )
        return self.registry.get_default()

    def _select_model(self, tier: int) -> str:
        """Select an Ollama model based on the tier."""
        from nerv.hardware import PROFILE
        model_map = {
            0: os.environ.get("NERV_MODEL_TIER0", PROFILE["recommended_router_model"]),
            1: os.environ.get("NERV_MODEL_TIER1", "qwen2.5:7b"),
            2: os.environ.get("NERV_MODEL_TIER2", "qwen2.5:14b"),
            3: os.environ.get("NERV_MODEL_TIER3", "qwen2.5:32b"),
        }
        model = model_map.get(tier, model_map[0])
        logger.debug("Selected model for tier %d: %s", tier, model)
        return model

    def _remember_turn(self, user_message: str, assistant_message: str) -> None:
        """Persist a direct user/assistant turn without invoking the model."""
        self.memory.append_message("user", user_message)
        self.memory.append_message("assistant", assistant_message)

    async def _call_model(
        self,
        model: str,
        messages: list[dict[str, Any]],
        tools_schemas: list[dict[str, Any]] = None,
        sender: str = "",
        channel: str = "",
    ) -> str:
        """Call Ollama with the resolved messages payload and handle function calling loop."""
        url = f"{self.base_url}/api/chat"
        
        # Import the runtime hardware sniff results
        from nerv.hardware import PROFILE
        
        # We loop until the LLM stops calling tools and provides a regular reply
        while True:
            options = {
                "num_predict": 1024,
            }
            options.update(PROFILE.get("ollama_options", {}))
            
            payload = {
                "model": model,
                "messages": messages,
                "stream": False,
                "options": options,
            }
            if tools_schemas:
                payload["tools"] = tools_schemas

            logger.debug("Calling model %s via %s (messages count: %d)", model, url, len(messages))
            response = await self._client.post(url, json=payload)
            response.raise_for_status()

            data = response.json()
            message_obj = data.get("message", {})
            content = message_obj.get("content", "")
            tool_calls = message_obj.get("tool_calls", [])

            # Append the assistant's action to history
            messages.append(message_obj)

            if tool_calls:
                # LLM wants to use tools - we must intercept and execute
                logger.info("Model requested %d tool calls", len(tool_calls))
                from nerv.tools.builtins import registry

                queued_actions: list[PendingAction] = []
                for tc in tool_calls:
                    func_details = tc.get("function", {})
                    func_name = func_details.get("name")
                    arguments = func_details.get("arguments", {})

                    tool_def = registry.get_tool(func_name)
                    if not tool_def:
                        # Agent hallucinated a tool
                        messages.append({
                            "role": "tool",
                            "content": f"Unknown tool: {func_name}",
                        })
                        continue

                    if tool_def.requires_confirmation:
                        pending_action = self._queue_pending_action(
                            func_name,
                            arguments,
                            sender=sender,
                            channel=channel,
                        )
                        queued_actions.append(pending_action)
                        tool_result_content = (
                            "[ACTION SUSPENDED] "
                            + self._format_pending_action(pending_action)
                        )
                        self._pending_notifications.append(
                            "⚠️ [SYSTEM BACKGROUND ALERT] "
                            f"Pending action #{pending_action.id} ({func_name}) is waiting for confirmation."
                        )
                        logger.warning("Tool %s suspended for confirmation.", func_name)
                    else:
                        # Execute natively
                        logger.debug("Executing tool %s with args %s", func_name, arguments)
                        if inspect.iscoroutinefunction(tool_def.func):
                            result = await tool_def.func(**arguments)
                        else:
                            result = tool_def.func(**arguments)
                        tool_result_content = result.content

                    # Send the result back to the model
                    messages.append({
                        "role": "tool",
                        "content": tool_result_content,
                    })

                if queued_actions:
                    return self._build_pending_action_reply(queued_actions)

                # Loop continues, querying the LLM with the new context
                continue
                
            # If no tool calls, it's a solid final answer.
            if not content:
                return "I received an empty response from the model."

            return content

    def _queue_pending_action(
        self,
        tool_name: str,
        arguments: dict[str, Any],
        sender: str,
        channel: str,
    ) -> PendingAction:
        """Store a suspended action for later confirmation."""
        pending_action = PendingAction(
            id=self._next_pending_action_id,
            tool_name=tool_name,
            arguments=dict(arguments),
            sender=sender,
            channel=channel,
        )
        self._next_pending_action_id += 1
        self._pending_actions.append(pending_action)
        return pending_action

    def _format_pending_action(self, action: PendingAction) -> str:
        """Render a user-facing summary of a pending action."""
        if action.tool_name == "desktop_control":
            verb = action.arguments.get("action", "desktop action")
            details = []
            for key in ("application", "url", "text", "key", "modifiers"):
                value = str(action.arguments.get(key, "")).strip()
                if value:
                    details.append(f"{key}={value}")
            detail_text = f" ({', '.join(details)})" if details else ""
            summary = f"Pending action #{action.id}: desktop_control -> {verb}{detail_text}"
        elif action.tool_name == "screenshot":
            output_path = str(action.arguments.get("output_path", "")).strip() or "(auto path)"
            summary = f"Pending action #{action.id}: screenshot -> save to {output_path}"
        else:
            summary = (
                f"Pending action #{action.id}: {action.tool_name} "
                f"with arguments {json.dumps(action.arguments, ensure_ascii=False)}"
            )
        return summary

    def _build_pending_action_reply(self, actions: list[PendingAction]) -> str:
        """Create a deterministic user reply for queued high-risk actions."""
        lines = ["I queued the following high-risk action(s) for confirmation:"]
        lines.extend(f"- {self._format_pending_action(action)}" for action in actions)
        lines.append('Reply with "confirm <id>" to run one, or "cancel <id>" to discard it.')
        return "\n".join(lines)

    def _visible_pending_actions(self, sender: str, channel: str) -> list[PendingAction]:
        """Return pending actions scoped to the current sender/channel."""
        if sender or channel:
            return [
                action
                for action in self._pending_actions
                if (not sender or action.sender == sender)
                and (not channel or action.channel == channel)
            ]
        return list(self._pending_actions)

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

        if action_id:
            target = next((action for action in visible_actions if action.id == int(action_id)), None)
            if target is None:
                return f"I could not find pending action #{action_id}."
        elif len(visible_actions) == 1:
            target = visible_actions[0]
        else:
            ids = ", ".join(str(action.id) for action in visible_actions)
            return f"Multiple pending actions are waiting: {ids}. Reply with confirm <id> or cancel <id>."

        if command in CANCEL_COMMANDS:
            self._pending_actions = [action for action in self._pending_actions if action.id != target.id]
            return f"Cancelled pending action #{target.id}: {self._format_pending_action(target)}"

        if command in CONFIRM_COMMANDS:
            return await self._execute_pending_action(target)

        return None

    async def _execute_pending_action(self, action: PendingAction) -> str:
        """Run a previously approved pending action."""
        from nerv.tools.builtins import registry

        tool_def = registry.get_tool(action.tool_name)
        self._pending_actions = [
            pending_action
            for pending_action in self._pending_actions
            if pending_action.id != action.id
        ]

        if not tool_def:
            return (
                f"Pending action #{action.id} could not be executed because tool "
                f"{action.tool_name} is unavailable."
            )

        if inspect.iscoroutinefunction(tool_def.func):
            result = await tool_def.func(**action.arguments)
        else:
            result = tool_def.func(**action.arguments)

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
        if lower in {"show workflow reviews", "list workflow reviews", "show review queue"}:
            items = self.review_queue.list_reviews(status="pending")
            if not items:
                return "Workflow review queue is empty."
            lines = ["Pending workflow reviews:"]
            for item in items:
                lines.append(
                    f"- {item.review_id}: {item.workflow.name} — {item.workflow.description}"
                )
            lines.append('Reply with "approve workflow <review_id>" or "reject workflow <review_id>".')
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
                item = self.review_queue.reject_workflow(review_id, reviewer_notes=notes)
            except ValueError as e:
                return str(e)
            return f"Rejected workflow review {item.review_id}."

        return None

    async def check_background_tasks(self) -> str | None:
        """Called periodically by the Gateway heartbeat to check for pending actions.
        
        For the MVP, we scan for any suspended actions in the pending_actions queue
        and remind the user. In real scenarios, this could trigger background LLM sub-tasks.
        """
        # A simple hook to notify users if they forgot to approve something.
        if hasattr(self, "_pending_notifications") and self._pending_notifications:
            notification = self._pending_notifications.pop(0)
            return notification
        return None
