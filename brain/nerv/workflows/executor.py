"""Deterministic multi-step workflow executor.

Steps run structurally (one tool / reasoning step at a time). LLM help — argument
extraction and reasoning — flows through the unified client, so the executor is
backend-agnostic (claude-cli / ollama / litellm). Runs are persisted per-step to
a TaskRun, so an interrupted workflow can be resumed instead of restarting.

Tool steps go through the same confirmation gate as an agent's ad-hoc tool
use (see ``_call_tool_gated``): a step that needs approval is queued as a
pending action and reported, never run silently. Steps that need approval and
whose arguments are known before the run starts (the browser verbs) are asked
for once, as a whole run, instead of once per step (see ``execute``).
"""

import asyncio
import hashlib
import inspect
import json
import logging
import os
import re
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from nerv.capabilities import capability_registry
from nerv.llm import complete
from nerv.memory.manager import DEFAULT_SCOPE, active_scope
from nerv.observability import log_event, timed
from nerv.security import confirmation_reason, confirmed_action
from nerv.tasks import TaskRunStore
from nerv.tasks.store import DONE, FAILED, RUNNING, SUSPENDED
from nerv.workflows.models import WorkflowDefinition

if TYPE_CHECKING:
    from nerv.orchestrator.orchestrator import Orchestrator

logger = logging.getLogger(__name__)

STEP_RETRY_CAP_SECONDS = 8

# The tool a whole-run approval is queued under; the orchestrator calls it with the
# same arguments when the user confirms.
APPROVAL_TOOL = "execute_workflow"
# Longest step line shown in an approval prompt (the full call is still bound to it).
_SHOWN_STEP_CHARS = 160


class SuspendedText(str):
    """Text standing in for a tool call that was queued for confirmation, not run."""

    pending_id: int

    def __new__(cls, text: str, pending_id: int) -> "SuspendedText":
        obj = super().__new__(cls, text)
        obj.pending_id = pending_id
        return obj

    def __getnewargs__(self) -> tuple[str, int]:
        # Lets copy/deepcopy rebuild it (``__new__`` takes two arguments).
        return (str(self), self.pending_id)


class WorkflowNeedsApproval(Exception):  # noqa: N818 (a pause, not a failure)
    """The run is waiting for the user; the confirmation is already queued.

    ``message`` is meant for the model that called the workflow, ``user_message``
    for the user. ``stale`` means an approval was given but no longer matches the
    workflow (it changed after the user was asked), so nothing ran.
    """

    def __init__(self, message: str, user_message: str, *, stale: bool = False) -> None:
        super().__init__(message)
        self.message = message
        self.user_message = user_message
        self.stale = stale


@dataclass(frozen=True)
class _PlannedCall:
    """A tool call whose arguments are known before the run starts."""

    idx: int
    line: str  # the step as shown to the user
    tool_name: str
    kwargs: dict[str, Any]
    needs_approval: bool


def _call_key(tool_name: str, kwargs: dict[str, Any]) -> str:
    """Identity of one exact tool call, to match it against what was approved."""
    return f"{tool_name}:{json.dumps(kwargs, sort_keys=True, ensure_ascii=False)}"


class WorkflowExecutor:
    """Execute a workflow step-by-step with persistence and per-step retries."""

    def __init__(self, orchestrator: "Orchestrator") -> None:
        self.orchestrator = orchestrator
        self._store = TaskRunStore(orchestrator._project_root)
        # Exact calls the user approved for the run in progress (see ``execute``).
        self._approved_calls: frozenset[str] = frozenset()

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
        """Run (or resume) the workflow, persisting progress after every step.

        Steps that need confirmation and whose arguments are fixed up front are
        approved once for the whole run: the first call queues a single pending
        action listing them and raises :class:`WorkflowNeedsApproval`; when the user
        confirms it, the orchestrator calls this again and exactly those calls run
        without further prompts. A step that is only gated while running (the model
        chooses its arguments) stops the run with the same exception.
        """
        self._approved_calls = self._preflight(workflow, initial_input, run_id)
        try:
            return await self._execute_steps(workflow, initial_input, run_id)
        finally:
            self._approved_calls = frozenset()

    def _preflight(
        self, workflow: WorkflowDefinition, initial_input: str, run_id: str | None
    ) -> frozenset[str]:
        """Settle approval for the whole run before any step is executed.

        Returns the exact calls the user has already approved for this run (empty
        when nothing needs approval). Otherwise queues one pending action listing
        every step that needs it and raises WorkflowNeedsApproval. A confirmed
        approval only counts while the workflow still plans exactly what the user
        was shown; if it changed since, the user is asked again.
        """
        run = self._store.get(run_id) if run_id else None
        run_input = run.initial_input if run else initial_input
        if run:
            steps = [(s.idx, s.description) for s in run.steps if s.status != DONE]
        else:
            steps = [
                (i, self._step_text(s)) for i, s in enumerate(workflow.steps, start=1)
            ]
        gated = [c for c in self._plan(steps, run_input) if c.needs_approval]
        if not gated:
            return frozenset()

        reason = self._approval_reason(workflow.name, gated)
        arguments = {"workflow_name": workflow.name, "inputs": run_input}
        confirmed = confirmed_action()
        stale = False
        if (
            confirmed is not None
            and confirmed.tool_name == APPROVAL_TOOL
            and confirmed.arguments == arguments
        ):
            if confirmed.reason == reason:
                return frozenset(_call_key(c.tool_name, c.kwargs) for c in gated)
            stale = True  # the user approved a plan that no longer matches

        scope = active_scope()
        record = self.orchestrator._pending_store.add(
            APPROVAL_TOOL,
            arguments,
            reason=reason,
            scope="" if scope == DEFAULT_SCOPE else scope,
        )
        logger.warning(
            "Workflow '%s' needs approval (pending #%d)", workflow.name, record.id
        )
        lead = (
            "The workflow changed after it was approved, so nothing ran. "
            if stale
            else ""
        )
        raise WorkflowNeedsApproval(
            f"[ACTION SUSPENDED #{record.id}] {lead}{reason}\n"
            "This action was NOT executed. The user must approve it in Nerv with "
            f"'confirm {record.id}' before it runs. Do not retry — tell the user "
            "what is pending and continue with anything else you can do safely.",
            f"{lead}{reason}\n"
            f'Reply "confirm {record.id}" to run it, or "cancel {record.id}" to '
            "discard it.",
            stale=stale,
        )

    def _plan(
        self, steps: list[tuple[int, str]], initial_input: str
    ) -> list[_PlannedCall]:
        """The tool calls whose arguments are fixed before the run starts.

        These are the direct browser verbs. Anything the model has to fill in while
        running is not planned: it is gated step by step when it is reached.
        """
        orch = self.orchestrator
        known_input = self._initial_input([f"Initial Input: {initial_input}"])
        planned: list[_PlannedCall] = []
        for idx, text in steps:
            tool_name, method_hint, args_hint = self._parse_step(text)
            resolution = capability_registry.resolve(tool_name) if tool_name else None
            if resolution is None:
                continue
            kwargs = self._direct_kwargs(method_hint, args_hint, known_input)
            if kwargs is None:
                continue
            tool_def = resolution.tool_def
            reason = confirmation_reason(
                tool_def.name,
                kwargs,
                orch._workspace_root,
                requires_confirmation=tool_def.requires_confirmation,
                project_root=orch._project_root,
            )
            shown = " ".join(text.replace("%inputs%", initial_input).split())
            if len(shown) > _SHOWN_STEP_CHARS:
                shown = shown[: _SHOWN_STEP_CHARS - 3] + "..."
            planned.append(
                _PlannedCall(
                    idx, f"{idx}. {shown}", tool_def.name, kwargs, reason is not None
                )
            )
        return planned

    @staticmethod
    def _approval_reason(workflow_name: str, gated: list[_PlannedCall]) -> str:
        """What the user is asked to approve; the plan digest binds it to the calls."""
        plan = [[c.idx, c.tool_name, c.kwargs] for c in gated]
        digest = hashlib.sha256(
            json.dumps(plan, sort_keys=True, ensure_ascii=False).encode("utf-8")
        ).hexdigest()[:12]
        lines = [
            f"Workflow '{workflow_name}' will run these {len(gated)} steps, which "
            f"need your approval (plan {digest}):"
        ]
        lines.extend(f"  {c.line}" for c in gated)
        return "\n".join(lines)

    async def _execute_steps(
        self,
        workflow: WorkflowDefinition,
        initial_input: str,
        run_id: str | None,
    ) -> str:
        """The run itself: one step after another, saving progress as it goes."""
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

            step.result = str(output)  # plain text: it is copied and saved as JSON
            if isinstance(output, SuspendedText):
                # The step was queued for its own confirmation, not run. Stop here
                # instead of marking it done and carrying on without it.
                step.status = SUSPENDED
                run.status = SUSPENDED
                self._store.save(run)
                self._notify(
                    f"[Workflow] Stopped at step {step.idx}: waiting for confirmation"
                )
                where = (
                    f"stopped at step {step.idx}/{total}, which needs its own "
                    "confirmation"
                )
                raise WorkflowNeedsApproval(
                    f"Workflow '{workflow.name}' {where}; the steps after it did "
                    f"not run.\n{output}",
                    f"Workflow '{workflow.name}' {where}. Reply "
                    f'"confirm {output.pending_id}" to run just that step, or '
                    f'"cancel {output.pending_id}" to discard it. The steps after '
                    "it did not run.",
                )
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
        return await self._call_tool_gated(tool_def, args)

    async def _call_tool_gated(self, tool_def: Any, kwargs: dict[str, Any]) -> str:
        """Run a tool, or queue it for confirmation per the sandbox policy.

        A call the user already approved as part of this run (see ``execute``) goes
        straight through; anything else is gated as before.
        """
        orch = self.orchestrator
        approved = _call_key(tool_def.name, kwargs) in self._approved_calls
        reason = (
            None
            if approved
            else confirmation_reason(
                tool_def.name,
                kwargs,
                orch._workspace_root,
                requires_confirmation=tool_def.requires_confirmation,
                project_root=orch._project_root,
            )
        )
        if reason:
            scope = active_scope()
            record = orch._pending_store.add(
                tool_def.name,
                kwargs,
                reason=reason,
                scope="" if scope == DEFAULT_SCOPE else scope,
            )
            logger.warning("Workflow tool %s suspended: %s", tool_def.name, reason)
            return SuspendedText(
                f"[ACTION SUSPENDED #{record.id}] {reason}. Not executed; the "
                f"user must reply 'confirm {record.id}' to run it.",
                record.id,
            )
        if inspect.iscoroutinefunction(tool_def.func):
            tool_result = await tool_def.func(**kwargs)
        else:
            tool_result = await asyncio.to_thread(tool_def.func, **kwargs)
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

    @staticmethod
    def _initial_input(history_context: list[str]) -> str:
        """The workflow's input, as recorded at the head of the step context."""
        for ctx in history_context:
            if ctx.startswith("Initial Input:"):
                return ctx.replace("Initial Input:", "").strip()
        return ""

    @staticmethod
    def _direct_kwargs(
        method_hint: str, args_hint: str, initial_input: str
    ) -> dict[str, Any] | None:
        """Arguments for a browser-style verb, or None if the model must supply them."""
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
        return {"action": method_hint, **param_mappings[method_hint]}

    async def _try_direct_call(
        self,
        tool_def: Any,
        method_hint: str,
        args_hint: str,
        history_context: list[str],
    ) -> str | None:
        """Directly call a tool for simple deterministic browser-style verbs."""
        kwargs = self._direct_kwargs(
            method_hint, args_hint, self._initial_input(history_context)
        )
        if kwargs is None:
            return None

        try:
            self._notify(f"[Workflow] Direct call: {tool_def.name}.{method_hint}")
            return await self._call_tool_gated(tool_def, kwargs)
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
