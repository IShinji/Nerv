import asyncio
import inspect
import logging
from typing import Any

from nerv.capabilities import capability_registry
from nerv.workflows.models import WorkflowDefinition

logger = logging.getLogger(__name__)


class WorkflowExecutor:
    """Executes a workflow by stepping through it structurally, using a fast LLM to parse arguments."""

    def __init__(self, orchestrator: "Orchestrator") -> None:
        self.orchestrator = orchestrator

    def _get_raw_model(self) -> str:
        """Get the tier-0 model name stripped of the local: prefix for direct Ollama calls."""
        model = self.orchestrator._select_model(0)
        if model.startswith("local:"):
            return model[6:]
        return model

    async def execute(self, workflow: WorkflowDefinition, initial_input: str) -> str:
        """Run the workflow step-by-step."""
        # 1. Notify the user the workflow started
        self._notify(f"[Workflow] Started: {workflow.name}")
        
        model = self._get_raw_model()
        history_context: list[str] = [f"Initial Input: {initial_input}"]
        
        for idx, step_desc in enumerate(workflow.steps, start=1):
            if isinstance(step_desc, dict):
                step_str = step_desc.get("description", str(step_desc))
            else:
                step_str = str(step_desc)

            self._notify(f"[Workflow] Step {idx}/{len(workflow.steps)}: {step_str[:80]}...")
            
            # ── Parse step format: "tool_name.method -> args" or "tool_name action" ──
            tool_name, method_hint, args_hint = self._parse_step(step_str)

            resolution = capability_registry.resolve(tool_name) if tool_name else None

            if resolution:
                step_output = await self._execute_tool_step(
                    resolution.tool_def, method_hint, args_hint,
                    step_str, idx, workflow.name, model, history_context,
                )
            else:
                step_output = await self._execute_reasoning_step(
                    step_str, idx, workflow.name, model, history_context,
                )

            history_context.append(f"Step {idx} Result: {step_output[:2000]}")

        self._notify(f"[Workflow] Finished {workflow.name}")
        
        # Return last few context fragments for brevity
        return "\n".join(history_context[-3:])

    def _parse_step(self, step_str: str) -> tuple[str | None, str, str]:
        """Parse a workflow step description into (tool_name, method_hint, args_hint).

        Supported formats:
        - "chrome_browser.open_url -> https://gemini.google.com"
        - "browser.interactive.open_url -> https://gemini.google.com"
        - "chrome_browser.wait_for_idle"
        - "web_search query about AI models"
        """
        stripped = step_str.strip()
        if not stripped:
            return None, "", ""

        if "->" in stripped:
            left, right = stripped.split("->", maxsplit=1)
            callable_name = left.strip()
            resolution, method_hint = capability_registry.resolve_step_target(callable_name)
            if resolution is not None:
                return resolution.requested_name, method_hint, right.strip()

        first_word = stripped.split()[0]
        resolution, method_hint = capability_registry.resolve_step_target(first_word)
        if resolution is not None:
            remainder = stripped[len(first_word):].strip()
            return resolution.requested_name, method_hint, remainder

        return None, "", step_str

    async def _execute_tool_step(
        self,
        tool_def: Any,
        method_hint: str,
        args_hint: str,
        step_str: str,
        step_idx: int,
        workflow_name: str,
        model: str,
        history_context: list[str],
    ) -> str:
        """Execute a workflow step that targets a specific tool."""
        # Try direct argument injection first (for simple cases like open_url -> URL)
        if method_hint:
            direct_result = await self._try_direct_call(tool_def, method_hint, args_hint, history_context)
            if direct_result is not None:
                return direct_result

        # Fall back to LLM-assisted parameter extraction
        tool_schema = tool_def.schema
        messages = [
            {
                "role": "system",
                "content": (
                    f"You are executing step {step_idx} of workflow '{workflow_name}'.\n"
                    f"Step instructions: {step_str}\n"
                    "You must invoke the provided tool to accomplish this step using the context provided."
                ),
            },
            {
                "role": "user",
                "content": "Context history:\n" + "\n".join(history_context[-5:])
            }
        ]

        logger.info("Executing workflow step %d via LLM-assisted tool call: %s", step_idx, tool_def.name)

        url = f"{self.orchestrator.base_url}/api/chat"
        payload = {
            "model": model,
            "messages": messages,
            "stream": False,
            "tools": [tool_schema]
        }

        response = await self.orchestrator._client.post(url, json=payload, timeout=120)
        response.raise_for_status()
        data = response.json()
        message_obj = data.get("message", {})
        
        tool_calls = message_obj.get("tool_calls", [])
        
        if tool_calls:
            tc = tool_calls[0]
            func_details = tc.get("function", {})
            args = func_details.get("arguments", {})
            
            self._notify(f"[Workflow] Calling {tool_def.name}...")
            
            if inspect.iscoroutinefunction(tool_def.func):
                result = await tool_def.func(**args)
            else:
                result = tool_def.func(**args)
                
            return result.content
        else:
            return message_obj.get("content", "")

    async def _try_direct_call(
        self,
        tool_def: Any,
        method_hint: str,
        args_hint: str,
        history_context: list[str],
    ) -> str | None:
        """Try to directly call a tool without LLM assistance for simple, deterministic steps."""
        # Replace %inputs% placeholder with the initial user input
        initial_input = ""
        for ctx in history_context:
            if ctx.startswith("Initial Input:"):
                initial_input = ctx.replace("Initial Input:", "").strip()
                break

        resolved_args = args_hint.replace("%inputs%", initial_input)

        # Map common method hints to tool parameter names
        param_mappings = {
            "open_url": {"url": resolved_args},
            "fill_prompt": {"text": resolved_args},
            "type_text": {"text": resolved_args},
            "click_element": {"selector": resolved_args},
            "wait_for_selector": {"selector": resolved_args},
            "wait": {"seconds": int(resolved_args) if resolved_args.isdigit() else 5},
            "extract_text": {"selector": resolved_args},
            "get_page_text": {},
            "wait_for_idle": {"timeout_secs": 60},
            "submit_prompt": {},
        }

        if method_hint not in param_mappings:
            return None

        kwargs = param_mappings[method_hint]

        # For methods that map to a single tool call, try direct invocation
        try:
            self._notify(f"[Workflow] Direct call: {tool_def.name}.{method_hint}")
            if inspect.iscoroutinefunction(tool_def.func):
                result = await tool_def.func(action=method_hint, **kwargs)
            else:
                result = tool_def.func(action=method_hint, **kwargs)
            return result.content
        except TypeError:
            # Parameter mismatch — fall back to LLM-assisted call
            logger.debug("Direct call failed for %s.%s, falling back to LLM", tool_def.name, method_hint)
            return None

    async def _execute_reasoning_step(
        self,
        step_str: str,
        step_idx: int,
        workflow_name: str,
        model: str,
        history_context: list[str],
    ) -> str:
        """Execute a pure reasoning step (no specific tool target)."""
        messages = [
            {
                "role": "system",
                "content": (
                    f"You are evaluating step {step_idx} of workflow '{workflow_name}'.\n"
                    f"Step instructions: {step_str}\n"
                    "Process the prior outputs and generate the step result."
                ),
            },
            {
                "role": "user",
                "content": "Context history:\n" + "\n".join(history_context[-5:])
            }
        ]
        
        url = f"{self.orchestrator.base_url}/api/chat"
        payload = {
            "model": model,
            "messages": messages,
            "stream": False,
        }
        response = await self.orchestrator._client.post(url, json=payload, timeout=120)
        response.raise_for_status()
        data = response.json()
        return data.get("message", {}).get("content", "")

    def _notify(self, message: str) -> None:
        """Push a background notification up to the UI."""
        logger.info(message)
        if hasattr(self.orchestrator, "_pending_notifications"):
            self.orchestrator._pending_notifications.append(message)
