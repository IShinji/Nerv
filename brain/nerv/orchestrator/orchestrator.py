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

import httpx

from nerv.models import RouteResult
from nerv.orchestrator.registry import AgentDefinition, AgentRegistry

logger = logging.getLogger(__name__)

DEFAULT_OLLAMA_URL = "http://localhost:11434"
OLLAMA_TIMEOUT = 60.0


class Orchestrator:
    """Dispatch tasks to agents based on routing decisions."""

    def __init__(self, project_root: Path) -> None:
        self.registry = AgentRegistry(project_root)
        self.base_url = os.environ.get("NERV_OLLAMA_URL", DEFAULT_OLLAMA_URL)
        self._client = httpx.AsyncClient(timeout=OLLAMA_TIMEOUT)
        logger.info("Orchestrator initialized with %d agents", len(self.registry.list_agents()))

    async def dispatch(self, message: str, route_result: RouteResult) -> str:
        """Dispatch a message to the appropriate agent and return the response.

        Args:
            message: The original user message.
            route_result: Classification result from the Router.

        Returns:
            The agent's response text.
        """
        # If the router already provided a direct reply, use it
        if route_result.reply:
            return route_result.reply

        # Find the matching agent
        agent = self._find_agent(route_result)
        logger.info(
            "Dispatching to agent '%s' (intent=%s, tier=%d)",
            agent.name,
            route_result.intent,
            route_result.model_tier,
        )

        # Select model based on tier
        model = self._select_model(route_result.model_tier)

        # Call the model with the agent's system prompt
        try:
            response = await self._call_model(model, agent.system_prompt, message)
            return response
        except Exception as e:
            logger.error("Model call failed: %s", e)
            return f"I encountered an error while processing your request: {e}"

    def _find_agent(self, route_result: RouteResult) -> AgentDefinition:
        """Find the best matching agent for the routing result."""
        # Try exact match first
        agent = self.registry.find_by_type(route_result.agent_type)
        if agent:
            return agent

        # Fallback to default
        logger.warning(
            "No agent found for type '%s', using default",
            route_result.agent_type,
        )
        return self.registry.get_default()

    def _select_model(self, tier: int) -> str:
        """Select an Ollama model based on the tier.

        For Phase 1 MVP, all tiers use the local Ollama model.
        Future: tier 1+ routes to API providers via litellm.
        """
        # Read from env or use defaults per tier
        model_map = {
            0: os.environ.get("NERV_MODEL_TIER0", "qwen2.5:3b"),
            1: os.environ.get("NERV_MODEL_TIER1", "qwen2.5:7b"),
            2: os.environ.get("NERV_MODEL_TIER2", "qwen2.5:14b"),
            3: os.environ.get("NERV_MODEL_TIER3", "qwen2.5:32b"),
        }
        model = model_map.get(tier, model_map[0])
        logger.debug("Selected model for tier %d: %s", tier, model)
        return model

    async def _call_model(
        self, model: str, system_prompt: str, user_message: str
    ) -> str:
        """Call Ollama with the agent's system prompt and user message."""
        url = f"{self.base_url}/api/chat"
        payload = {
            "model": model,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": user_message},
            ],
            "stream": False,
            "options": {
                "num_predict": 1024,
            },
        }

        logger.debug("Calling model %s via %s", model, url)
        response = await self._client.post(url, json=payload)
        response.raise_for_status()

        data = response.json()
        content = data.get("message", {}).get("content", "")

        if not content:
            return "I received an empty response from the model."

        return content
