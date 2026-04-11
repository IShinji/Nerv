"""Orchestrator — receives routing decisions and dispatches to agents.

This is the central coordinator that:
1. Receives a RouteResult from the Router
2. Finds the matching agent in the registry
3. Calls the appropriate model with the agent's system prompt
4. Returns the response
"""

import json
import logging
import os
from pathlib import Path
from typing import Any

import httpx

from nerv.memory.context import ContextManager
from nerv.memory.manager import MemoryManager
from nerv.models import RouteResult
from nerv.orchestrator.registry import AgentDefinition, AgentRegistry

logger = logging.getLogger(__name__)

DEFAULT_OLLAMA_URL = "http://localhost:11434"
OLLAMA_TIMEOUT = 60.0


class Orchestrator:
    """Dispatch tasks to agents based on routing decisions."""

    def __init__(self, project_root: Path) -> None:
        self.registry = AgentRegistry(project_root)
        self.memory = MemoryManager(project_root)
        self.context_manager = ContextManager(self.memory)
        self.base_url = os.environ.get("NERV_OLLAMA_URL", DEFAULT_OLLAMA_URL)
        self._client = httpx.AsyncClient(timeout=OLLAMA_TIMEOUT)
        logger.info("Orchestrator initialized with %d agents", len(self.registry.list_agents()))

    async def dispatch(self, message: str, route_result: RouteResult) -> str:
        """Dispatch a message to the appropriate agent and return the response."""
        # If the router already provided a direct reply, use it
        if route_result.reply:
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
        messages = self.context_manager.build_messages(agent, message, max_history_msg=10)

        # Build tools payload if agent has any
        tools_schemas = None
        if agent.tools:
            from nerv.tools.builtins import registry
            tools_schemas = registry.get_schemas(agent.tools)

        # Call the model
        try:
            response = await self._call_model(model, messages, tools_schemas)
            
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
        model_map = {
            0: os.environ.get("NERV_MODEL_TIER0", "qwen2.5:3b"),
            1: os.environ.get("NERV_MODEL_TIER1", "qwen2.5:7b"),
            2: os.environ.get("NERV_MODEL_TIER2", "qwen2.5:14b"),
            3: os.environ.get("NERV_MODEL_TIER3", "qwen2.5:32b"),
        }
        model = model_map.get(tier, model_map[0])
        logger.debug("Selected model for tier %d: %s", tier, model)
        return model

    async def _call_model(self, model: str, messages: list[dict[str, Any]], tools_schemas: list[dict[str, Any]] = None) -> str:
        """Call Ollama with the resolved messages payload and handle function calling loop."""
        url = f"{self.base_url}/api/chat"
        
        # We loop until the LLM stops calling tools and provides a regular reply
        while True:
            payload = {
                "model": model,
                "messages": messages,
                "stream": False,
                "options": {
                    "num_predict": 1024,
                },
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

                    # TODO: Implement the PENDING SUSPENSION check properly
                    if tool_def.requires_confirmation:
                        # For MVP: We mock the suspended queue reply to the model
                        tool_result_content = f"[ACTION SUSPENDED] The execution of {func_name} requires user confirmation. I have queued it in 'Pending Actions'. Inform the user."
                        logger.warning("Tool %s suspended for confirmation.", func_name)
                    else:
                        # Execute natively
                        logger.debug("Executing tool %s with args %s", func_name, arguments)
                        import inspect
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

                # Loop continues, querying the LLM with the new context
                continue
                
            # If no tool calls, it's a solid final answer.
            if not content:
                return "I received an empty response from the model."

            return content
