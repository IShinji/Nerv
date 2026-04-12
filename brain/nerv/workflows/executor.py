import asyncio
import json
import logging
from typing import Any

from nerv.workflows.models import WorkflowDefinition

logger = logging.getLogger(__name__)


class WorkflowExecutor:
    """Executes a workflow by stepping through it structurally, using a fast LLM to parse arguments."""

    def __init__(self, orchestrator: "Orchestrator") -> None:
        self.orchestrator = orchestrator

    async def execute(self, workflow: WorkflowDefinition, initial_input: str) -> str:
        """Run the workflow step-by-step."""
        from nerv.tools.builtins import registry

        # 1. Notify the user the workflow started
        self._notify(f"[Workflow] Started: {workflow.name}")
        
        # Determine base model for execution - use the fast tier0 model for deterministic tool extraction
        model = self.orchestrator._select_model(0)

        history_context: list[str] = [f"Initial Input: {initial_input}"]
        
        for idx, step_desc in enumerate(workflow.steps, start=1):
            if isinstance(step_desc, dict):
                # Handle future cases where steps are parsed into rich dicts
                step_str = step_desc.get("description", str(step_desc))
            else:
                step_str = str(step_desc)

            self._notify(f"[Workflow] Step {idx}/{len(workflow.steps)}: {step_str[:50]}...")
            
            # Extract target tool if explicitly named in the format "tool.method"
            target_tool = None
            first_word = step_str.split()[0]
            if "." in first_word or "_" in first_word:
                tool_cand = registry.get_tool(first_word)
                if tool_cand:
                    target_tool = tool_cand

            if target_tool:
                # Constrain execution strictly to this single tool
                tool_schema = target_tool.schema()
                messages = [
                    {
                        "role": "system",
                        "content": (
                            f"You are executing step {idx} of workflow '{workflow.name}'.\n"
                            f"Step instructions: {step_str}\n"
                            "You must invoke the provided tool to accomplish this step using the context provided."
                        ),
                    },
                    {
                        "role": "user",
                        "content": "Context history:\n" + "\n".join(history_context)
                    }
                ]
                
                logger.info("Executing workflow step %d constrained to tool %s", idx, target_tool.name)
                
                # We do a direct post to the model forcing it to use the tool
                # Using httpx directly from orchestrator
                url = f"{self.orchestrator.base_url}/api/chat"
                payload = {
                    "model": model,
                    "messages": messages,
                    "stream": False,
                    "tools": [tool_schema]
                }
                
                response = await self.orchestrator._client.post(url, json=payload)
                response.raise_for_status()
                data = response.json()
                message_obj = data.get("message", {})
                
                tool_calls = message_obj.get("tool_calls", [])
                step_output = ""
                
                if tool_calls:
                    tc = tool_calls[0] # Take the first one
                    func_details = tc.get("function", {})
                    args = func_details.get("arguments", {})
                    
                    self._notify(f"[Workflow] Calling {target_tool.name}...")
                    
                    import inspect
                    if inspect.iscoroutinefunction(target_tool.func):
                        result = await target_tool.func(**args)
                    else:
                        result = target_tool.func(**args)
                        
                    step_output = result.content
                else:
                    # Model hallucinated and didn't call the tool!
                    # Fallback to its generated text
                    step_output = message_obj.get("content", "")
                    
                history_context.append(f"Step {idx} Result ({target_tool.name}): {step_output}")
                
            else:
                # No specific tool targeted, just let the LLM reason normally for the step
                messages = [
                    {
                        "role": "system",
                        "content": (
                            f"You are evaluating step {idx} of workflow '{workflow.name}'.\n"
                            f"Step instructions: {step_str}\n"
                            "Process the prior outputs and generate the step result."
                        ),
                    },
                    {
                        "role": "user",
                        "content": "Context history:\n" + "\n".join(history_context)
                    }
                ]
                
                url = f"{self.orchestrator.base_url}/api/chat"
                payload = {
                    "model": model,
                    "messages": messages,
                    "stream": False,
                }
                response = await self.orchestrator._client.post(url, json=payload)
                response.raise_for_status()
                data = response.json()
                step_output = data.get("message", {}).get("content", "")
                
                history_context.append(f"Step {idx} Reasoning: {step_output}")

        self._notify(f"[Workflow] Finished {workflow.name}")
        
        # Final summarization
        return "\n".join(history_context[-3:])  # Return last few context fragments for brevity

    def _notify(self, message: str) -> None:
        """Push a background notification up to the UI."""
        logger.info(message)
        if hasattr(self.orchestrator, "_pending_notifications"):
            self.orchestrator._pending_notifications.append(message)
