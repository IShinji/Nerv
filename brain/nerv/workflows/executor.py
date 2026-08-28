"""Deterministic multi-step workflow executor.

Steps run structurally (one tool / reasoning step at a time). LLM help — argument
extraction and reasoning — flows through the unified client, so the executor is
backend-agnostic (claude-cli / ollama / litellm). Runs are persisted per-step to
a TaskRun, so an interrupted workflow can be resumed instead of restarting.

Note: workflow tool steps execute directly. Workflows are pre-approved sequences
(authored or passed through the review queue), unlike an agent's ad-hoc tool use
which is sandbox-gated in the orchestrator.
"""

import asyncio
import inspect
import json
import logging
import os
import re
from typing import TYPE_CHECKING, Any

from nerv.capabilities import capability_registry
from nerv.llm import complete
from nerv.observability import log_event, timed
from nerv.tasks import TaskRunStore
from nerv.tasks.store import DONE, FAILED, RUNNING
from nerv.workflows.models import WorkflowDefinition

if TYPE_CHECKING:
    from nerv.orchestrator.orchestrator import Orchestrator

logger = logging.getLogger(__name__)

STEP_RETRY_CAP_SECONDS = 8


class WorkflowExecutor:
    """Execute a workflow step-by-step with persistence and per-step retries."""

    def __init__(self, orchestrator: "Orchestrator") -> None:
        self.orchestrator = orchestrator
        self._store = TaskRunStore(orchestrator._project_root)

    def _model(self) -> str:
        """Model string for executor LLM help (tier 0 — cheap, provider-prefixed)."""
        return self.orchestrator._select_model(0)

    @staticmethod
    def _step_text(step_desc: Any) -> str:
        if isinstance(step_desc, dict):
            return step_desc.get("description", str(step_desc))
        return str(getattr(step_desc, "description", step_desc) or step_desc)

    async def execute(
        self,
        workflow: WorkflowDefinition,
        initial_input: str,
        run_id: str | None = None,
    ) -> str:
        """Run (or resume) the workflow, persisting progress after every step."""
        run = self._store.get(run_id) if run_id else None
        if run is None:
            descriptions = [self._step_text(s) for s in workflow.steps]
            run = self._store.create(workflow.name, initial_input, descriptions)
            self._notify(f"[Workflow] Started: {workflow.name} (run {run.id})")
        else:
            self._notify(f"[Workflow] Resuming: {workflow.name} (run {run.id})")

        run.status = RUNNING
        model = self._model()

        # Rebuild context from any already-completed steps (resume support).
        history_context = [f"Initial Input: {run.initial_input}"]
        for step in run.steps:
            if step.status == DONE:
                history_context.append(f"Step {step.idx} Result: {step.result[:2000]}")

        total = len(run.steps)
        for step in run.steps:
            if step.status == DONE:
                continue

            step.status = RUNNING
            self._store.save(run)
            self._notify(
                f"[Workflow] Step {step.idx}/{total}: {step.description[:80]}..."
            )

            try:
                output = await self._run_step_with_retries(
                    step, workflow.name, model, history_context
                )
            except Exception as exc:
                step.status = FAILED
                run.status = FAILED
                self._store.save(run)
                logger.error(
                    "Workflow '%s' failed at step %d: %s", workflow.name, step.idx, exc
                )
                self._notify(f"[Workflow] Failed at step {step.idx}: {exc}")
                return (
                    f"Workflow '{workflow.name}' failed at step {step.idx} "
                    f"({step.description[:60]}): {exc}. Progress saved as run {run.id}."
                )

            step.result = output
            step.status = DONE
            self._store.save(run)
            history_context.append(f"Step {step.idx} Result: {output[:2000]}")

        run.status = DONE
        self._store.save(run)
        self._notify(f"[Workflow] Finished {workflow.name}")
        return "\n".join(history_context[-3:])

    async def _run_step_with_retries(
        self,
        step: Any,
        workflow_name: str,
        model: str,
        history_context: list[str],
    ) -> str:
        """Execute one step, retrying transient failures with backoff."""
        attempts = max(1, int(os.environ.get("NERV_WORKFLOW_STEP_ATTEMPTS", "2")))
        last_error: Exception | None = None
        for attempt in range(1, attempts + 1):
            step.attempts = attempt
            try:
                with timed(
                    "workflow_step",
                    workflow=workflow_name,
                    step=step.idx,
                    attempt=attempt,
                ):
                    return await self._execute_step(
                        step.description,
                        step.idx,
                        workflow_name,
                        model,
                        history_context,
                    )
            except Exception as exc:
                last_error = exc
                logger.warning(
                    "Workflow step %d attempt %d/%d failed: %s",
                    step.idx,
                    attempt,
                    attempts,
                    exc,
                )
                if attempt < attempts:
                    await asyncio.sleep(min(2 ** (attempt - 1), STEP_RETRY_CAP_SECONDS))
        raise last_error if last_error else RuntimeError("step failed")

    async def _execute_step(
        self,
        step_str: str,
        step_idx: int,
        workflow_name: str,
        model: str,
        history_context: list[str],
    ) -> str:
        """Dispatch one step to a tool call or a reasoning step."""
        tool_name, method_hint, args_hint = self._parse_step(step_str)
        resolution = capability_registry.resolve(tool_name) if tool_name else None

        if resolution:
            return await self._execute_tool_step(
                resolution.tool_def,
                method_hint,
                args_hint,
                step_str,
                step_idx,
                workflow_name,
                model,
                history_context,
            )
        return await self._execute_reasoning_step(
            step_str, step_idx, workflow_name, model, history_context
        )

    def _parse_step(self, step_str: str) -> tuple[str | None, str, str]:
        """Parse a step into (tool_name, method_hint, args_hint).

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
            target = left.strip()
            resolution, method_hint = capability_registry.resolve_step_target(target)
            if resolution is not None:
                return resolution.requested_name, method_hint, right.strip()

        first_word = stripped.split()[0]
        resolution, method_hint = capability_registry.resolve_step_target(first_word)
        if resolution is not None:
            remainder = stripped[len(first_word) :].strip()
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
        """Execute a step that targets a specific tool."""
        # Fast path: deterministic direct calls (no LLM) for known browser verbs.
        if method_hint:
            direct_result = await self._try_direct_call(
                tool_def, method_hint, args_hint, history_context
            )
            if direct_result is not None:
                return direct_result

        # Otherwise ask the model for the tool's arguments as JSON, then invoke
        # the tool natively. This works across all backends (claude-cli does not
        # hand back structured tool calls, so we extract args explicitly).
        schema = tool_def.schema.get("function", {})
        parameters = schema.get("parameters", {})
        messages = [
            {
                "role": "system",
                "content": (
                    f"You are executing step {step_idx} of "
                    f"workflow '{workflow_name}'.\n"
                    f"Step instructions: {step_str}\n"
                    f"Produce the arguments for the tool '{schema.get('name')}' as a "
                    f"JSON object matching this parameter schema:\n"
                    f"{json.dumps(parameters, ensure_ascii=False)}\n"
                    "Fill values using the provided context. Output JSON only."
                ),
            },
            {
                "role": "user",
                "content": "Context history:\n" + "\n".join(history_context[-5:]),
            },
        ]

        logger.info("Workflow step %d: extracting args for %s", step_idx, tool_def.name)
        result = await complete(
            messages,
            model=model,
            json_mode=True,
            project_root=self.orchestrator._project_root,
        )
        args = self._parse_json_args(result.content)

        self._notify(f"[Workflow] Calling {tool_def.name}...")
        if inspect.iscoroutinefunction(tool_def.func):
            tool_result = await tool_def.func(**args)
        else:
            tool_result = tool_def.func(**args)
        return tool_result.content

    @staticmethod
    def _parse_json_args(content: str) -> dict[str, Any]:
        """Extract a JSON argument object from model output (tolerant)."""
        content = content.strip()
        try:
            parsed = json.loads(content)
            return parsed if isinstance(parsed, dict) else {}
        except json.JSONDecodeError:
            match = re.search(r"\{.*\}", content, re.DOTALL)
            if match:
                try:
                    parsed = json.loads(match.group())
                    return parsed if isinstance(parsed, dict) else {}
                except json.JSONDecodeError:
                    return {}
            return {}

    async def _try_direct_call(
        self,
        tool_def: Any,
        method_hint: str,
        args_hint: str,
        history_context: list[str],
    ) -> str | None:
        """Directly call a tool for simple deterministic browser-style verbs."""
        initial_input = ""
        for ctx in history_context:
            if ctx.startswith("Initial Input:"):
                initial_input = ctx.replace("Initial Input:", "").strip()
                break

        resolved_args = args_hint.replace("%inputs%", initial_input)

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
        try:
            self._notify(f"[Workflow] Direct call: {tool_def.name}.{method_hint}")
            if inspect.iscoroutinefunction(tool_def.func):
                result = await tool_def.func(action=method_hint, **kwargs)
            else:
                result = tool_def.func(action=method_hint, **kwargs)
            return result.content
        except TypeError:
            logger.debug(
                "Direct call failed for %s.%s, falling back to LLM",
                tool_def.name,
                method_hint,
            )
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
                    f"You are evaluating step {step_idx} of "
                    f"workflow '{workflow_name}'.\n"
                    f"Step instructions: {step_str}\n"
                    "Process the prior outputs and generate the step result."
                ),
            },
            {
                "role": "user",
                "content": "Context history:\n" + "\n".join(history_context[-5:]),
            },
        ]
        result = await complete(
            messages, model=model, project_root=self.orchestrator._project_root
        )
        return result.content

    def _notify(self, message: str) -> None:
        """Push a background notification up to the UI and trace it."""
        logger.info(message)
        log_event("workflow_notice", message=message)
        if hasattr(self.orchestrator, "_pending_notifications"):
            self.orchestrator._pending_notifications.append(message)
