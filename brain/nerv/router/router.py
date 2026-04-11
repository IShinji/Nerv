"""Router — intent classification using local Ollama model.

Calls Ollama HTTP API to classify user messages into intents,
complexity levels, and suggested model tiers. This is the first
stage of the message pipeline — designed to be fast and cheap (~200 tokens).
"""

import json
import logging
import os
from typing import Any

import httpx

from nerv.models import RouteResult
from nerv.router.prompt import ROUTER_SYSTEM_PROMPT, ROUTER_USER_TEMPLATE

logger = logging.getLogger(__name__)

from nerv.hardware import PROFILE

# Defaults — can be overridden by environment variables for flexibility
DEFAULT_OLLAMA_URL = "http://localhost:11434"
DEFAULT_MODEL = PROFILE["recommended_router_model"]
OLLAMA_TIMEOUT = 30.0


class Router:
    """Classify user messages using a local Ollama model."""

    def __init__(
        self,
        base_url: str | None = None,
        model: str | None = None,
    ) -> None:
        self.base_url = base_url or os.environ.get("NERV_OLLAMA_URL", DEFAULT_OLLAMA_URL)
        self.model = model or os.environ.get("NERV_ROUTER_MODEL", DEFAULT_MODEL)
        self._client = httpx.AsyncClient(timeout=OLLAMA_TIMEOUT)
        logger.info("Router initialized: model=%s, url=%s", self.model, self.base_url)

    async def classify(self, message: str) -> RouteResult:
        """Classify a user message into an intent + routing decision.

        Args:
            message: Raw user message text.

        Returns:
            RouteResult with intent, complexity, model_tier, agent_type, and reply.
        """
        if not message.strip():
            return RouteResult(
                intent="general",
                complexity="low",
                model_tier=0,
                agent_type="general",
                reply="I received an empty message. How can I help you?",
            )

        user_prompt = ROUTER_USER_TEMPLATE.format(message=message)

        try:
            result = await self._call_ollama(user_prompt)
            return result
        except Exception as e:
            logger.error("Ollama call failed, using fallback: %s", e)
            return self._fallback_classify(message)

    async def _call_ollama(self, user_prompt: str) -> RouteResult:
        """Call Ollama HTTP API for chat completion."""
        url = f"{self.base_url}/api/chat"
        options = {
            "temperature": 0.1,  # Low temp for consistent classification
            "num_predict": 256,  # Cap output tokens
        }
        # Merge in hardware optimizations (like num_thread, num_ctx)
        options.update(PROFILE.get("ollama_options", {}))
        
        payload = {
            "model": self.model,
            "messages": [
                {"role": "system", "content": ROUTER_SYSTEM_PROMPT},
                {"role": "user", "content": user_prompt},
            ],
            "stream": False,
            "format": "json",
            "options": options,
        }

        logger.debug("Calling Ollama: %s", url)
        response = await self._client.post(url, json=payload)
        response.raise_for_status()

        data = response.json()
        content = data.get("message", {}).get("content", "")
        logger.debug("Ollama raw response: %s", content)

        return self._parse_response(content)

    def _parse_response(self, content: str) -> RouteResult:
        """Parse the JSON response from the LLM."""
        try:
            parsed = json.loads(content)
            return RouteResult(**parsed)
        except (json.JSONDecodeError, TypeError, ValueError) as e:
            logger.warning("Failed to parse LLM response as JSON: %s — %s", content, e)
            # Try to extract JSON from the response if wrapped in text
            return self._extract_json(content)

    def _extract_json(self, content: str) -> RouteResult:
        """Try to extract JSON from a response that may contain extra text."""
        import re

        json_match = re.search(r"\{[^}]+\}", content)
        if json_match:
            try:
                parsed = json.loads(json_match.group())
                return RouteResult(**parsed)
            except (json.JSONDecodeError, TypeError, ValueError):
                pass

        logger.warning("Could not extract JSON from response, using fallback")
        return RouteResult(
            intent="general",
            complexity="low",
            model_tier=0,
            agent_type="general",
            reply="I understood your message but had trouble classifying it. Could you rephrase?",
        )

    def _fallback_classify(self, message: str) -> RouteResult:
        """Simple keyword-based fallback when Ollama is unavailable."""
        message_lower = message.lower()

        # Simple keyword matching as fallback
        code_keywords = ["code", "script", "function", "debug", "error", "bug", "program", "python", "rust", "javascript"]
        research_keywords = ["search", "find", "look up", "what is", "how does", "explain", "research"]
        writing_keywords = ["write", "essay", "article", "translate", "summarize", "email"]
        sysadmin_keywords = ["server", "deploy", "docker", "ssh", "nginx", "process", "disk", "memory"]

        if any(kw in message_lower for kw in code_keywords):
            return RouteResult(intent="code_generation", complexity="medium", model_tier=1, agent_type="coder", reply="")
        elif any(kw in message_lower for kw in research_keywords):
            return RouteResult(intent="research", complexity="medium", model_tier=1, agent_type="researcher", reply="")
        elif any(kw in message_lower for kw in writing_keywords):
            return RouteResult(intent="writing", complexity="medium", model_tier=1, agent_type="writer", reply="")
        elif any(kw in message_lower for kw in sysadmin_keywords):
            return RouteResult(intent="sysadmin", complexity="medium", model_tier=0, agent_type="sysadmin", reply="")
        else:
            return RouteResult(intent="general", complexity="low", model_tier=0, agent_type="general", reply="")
