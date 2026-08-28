"""Tests for the Router."""

from nerv.models import RouteResult
from nerv.router.router import Router


class TestRouterFallback:
    """Test the keyword-based fallback classifier."""

    def setup_method(self) -> None:
        self.router = Router()

    def test_code_intent_detected(self) -> None:
        result = self.router._fallback_classify("help me write a Python script")
        assert result.intent == "code_generation"
        assert result.agent_type == "coder"

    def test_research_intent_detected(self) -> None:
        result = self.router._fallback_classify("what is quantum computing")
        assert result.intent == "research"
        assert result.agent_type == "researcher"

    def test_writing_intent_detected(self) -> None:
        result = self.router._fallback_classify("write me an essay about AI")
        assert result.intent == "writing"
        assert result.agent_type == "writer"

    def test_sysadmin_intent_detected(self) -> None:
        result = self.router._fallback_classify("deploy this to the server")
        assert result.intent == "sysadmin"
        assert result.agent_type == "sysadmin"

    def test_general_intent_fallback(self) -> None:
        result = self.router._fallback_classify("hello there!")
        assert result.intent == "general"
        assert result.agent_type == "general"

    def test_empty_message_classified(self) -> None:
        result = self.router._fallback_classify("")
        assert result.intent == "general"


class TestRouterParsing:
    """Test JSON response parsing."""

    def setup_method(self) -> None:
        self.router = Router()

    def test_valid_json_parsed(self) -> None:
        content = '{"intent": "code_generation", "complexity": "medium", "model_tier": 1, "agent_type": "coder", "reply": ""}'
        result = self.router._parse_response(content)
        assert result.intent == "code_generation"
        assert result.model_tier == 1

    def test_json_with_extra_text_extracted(self) -> None:
        content = 'Here is the result: {"intent": "general", "complexity": "low", "model_tier": 0, "agent_type": "general", "reply": "hello"}'
        result = self.router._parse_response(content)
        assert result.intent == "general"

    def test_invalid_json_returns_fallback(self) -> None:
        content = "This is not JSON at all"
        result = self.router._parse_response(content)
        assert result.intent == "general"

    def test_general_intent_agent_type_is_normalized(self) -> None:
        result = RouteResult(
            intent="general",
            complexity="low",
            model_tier=0,
            agent_type="personal AI assistant",
            reply="Hello!",
        )
        sanitized = self.router._sanitize_route_result("hello", result)
        assert sanitized.agent_type == "general"

    def test_question_reply_is_suppressed(self) -> None:
        result = RouteResult(
            intent="general",
            complexity="low",
            model_tier=0,
            agent_type="search",
            reply="现在几点？",
        )
        sanitized = self.router._sanitize_route_result("现在几点", result)
        assert sanitized.reply == ""
        assert sanitized.agent_type == "general"

    def test_trivial_greeting_can_keep_direct_reply(self) -> None:
        result = RouteResult(
            intent="general",
            complexity="low",
            model_tier=0,
            agent_type="general",
            reply="你好，有什么可以帮你？",
        )
        sanitized = self.router._sanitize_route_result("你好", result)
        assert sanitized.reply == "你好，有什么可以帮你？"


class TestRouteResult:
    """Test the RouteResult model."""

    def test_default_values(self) -> None:
        result = RouteResult(intent="general")
        assert result.complexity == "low"
        assert result.model_tier == 0
        assert result.agent_type == "general"
        assert result.needs_capabilities == []
        assert result.preferred_execution_mode == "auto"
        assert result.reply == ""

    def test_serialization(self) -> None:
        result = RouteResult(
            intent="code_generation",
            complexity="high",
            model_tier=2,
            agent_type="coder",
            needs_capabilities=["filesystem.read", "local.exec"],
            preferred_execution_mode="auto",
            reply="I can help with that",
        )
        data = result.model_dump()
        assert data["intent"] == "code_generation"
        assert data["model_tier"] == 2
        assert data["needs_capabilities"] == ["filesystem.read", "local.exec"]
        assert data["preferred_execution_mode"] == "auto"
