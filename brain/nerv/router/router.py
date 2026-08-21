"""Router — intent classification via the configured model backend.

Classifies user messages into intents, complexity levels, and suggested model
tiers. This is the first stage of the message pipeline. The backend model is a
pure config switch (claude-cli / local Ollama / litellm) resolved through the
unified LLM client; a keyword fallback keeps routing working fully offline.
"""

import json
import logging
import re
from pathlib import Path

from nerv.config import get_router_model
from nerv.models import RouteResult
from nerv.router.prompt import ROUTER_SYSTEM_PROMPT, ROUTER_USER_TEMPLATE

logger = logging.getLogger(__name__)

ROUTER_TIMEOUT = 60.0

CANONICAL_AGENT_BY_INTENT = {
    "general": "general",
    "code_generation": "coder",
    "research": "researcher",
    "writing": "writer",
    "sysadmin": "sysadmin",
}

AGENT_ALIASES = {
    "general": "general",
    "assistant": "general",
    "personal_ai_assistant": "general",
    "chat": "general",
    "code_generation": "coder",
    "coder": "coder",
    "developer": "coder",
    "programmer": "coder",
    "software_developer": "coder",
    "research": "researcher",
    "researcher": "researcher",
    "search": "researcher",
    "analyst": "researcher",
    "writing": "writer",
    "writer": "writer",
    "translator": "writer",
    "summarizer": "writer",
    "sysadmin": "sysadmin",
    "system_admin": "sysadmin",
    "system_administrator": "sysadmin",
    "devops": "sysadmin",
}

DIRECT_REPLY_EXACT_MESSAGES = {
    "hi",
    "hello",
    "hey",
    "thanks",
    "thank you",
    "ok",
    "okay",
    "got it",
    "bye",
    "你好",
    "您好",
    "嗨",
    "哈喽",
    "谢谢",
    "好的",
    "收到",
    "再见",
    "对的",
    "嗯",
    "嗯嗯",
}

QUESTION_MARKERS = (
    "?",
    "？",
    "what",
    "when",
    "where",
    "why",
    "how",
    "who",
    "which",
    "time",
    "date",
    "几点",
    "几号",
    "几月",
    "多少",
    "什么",
    "怎么",
    "谁",
    "哪",
    "何时",
    "为什么",
)


class Router:
    """Classify user messages using the configured model backend."""

    def __init__(
        self,
        project_root: Path | None = None,
        model: str | None = None,
    ) -> None:
        self._project_root = project_root or Path.cwd()
        self.model = model or get_router_model(self._project_root)
        logger.info("Router initialized: model=%s", self.model)

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
            result = await self._classify_via_model(user_prompt)
            return self._sanitize_route_result(message, result)
        except Exception as e:
            logger.error("Router model call failed, using fallback: %s", e)
            return self._sanitize_route_result(message, self._fallback_classify(message))

    async def _classify_via_model(self, user_prompt: str) -> RouteResult:
        """Classify via the unified LLM client (provider chosen by config)."""
        from nerv.llm import complete

        messages = [
            {"role": "system", "content": ROUTER_SYSTEM_PROMPT},
            {"role": "user", "content": user_prompt},
        ]
        result = await complete(
            messages,
            model=self.model,
            temperature=0.1,
            json_mode=True,
            project_root=self._project_root,
            timeout=ROUTER_TIMEOUT,
        )
        logger.debug("Router raw response: %s", result.content)
        return self._parse_response(result.content)

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

    def _sanitize_route_result(self, message: str, result: RouteResult) -> RouteResult:
        """Normalize noisy classifier output into stable routing fields."""
        result.agent_type = self._normalize_agent_type(result.intent, result.agent_type)
        if not self._should_use_direct_reply(message, result.reply):
            result.reply = ""
        return result

    def _normalize_agent_type(self, intent: str, agent_type: str) -> str:
        """Map fuzzy agent names into stable built-in roles when possible."""
        normalized = re.sub(r"[^a-z0-9_]+", "_", agent_type.strip().lower())
        normalized = re.sub(r"_+", "_", normalized).strip("_")

        if intent == "general":
            return "general"

        if normalized in AGENT_ALIASES:
            return AGENT_ALIASES[normalized]

        if not normalized:
            return CANONICAL_AGENT_BY_INTENT.get(intent, "general")

        return normalized

    def _should_use_direct_reply(self, message: str, reply: str) -> bool:
        """Only allow direct router replies for trivial social messages."""
        if not reply.strip():
            return False

        normalized = " ".join(message.strip().lower().split())
        if not normalized:
            return True

        if any(marker in normalized for marker in QUESTION_MARKERS):
            return False

        return normalized in DIRECT_REPLY_EXACT_MESSAGES

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
