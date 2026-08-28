"""Registry and core types for Tools."""

import inspect
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any, TypeAlias


@dataclass
class ToolResult:
    """The result of executing a tool."""

    content: str
    is_error: bool = False
    is_pending: bool = (
        False  # Indicates the tool was paused/suspended waiting for user approval
    )


ToolFunction: TypeAlias = Callable[..., ToolResult]


@dataclass
class ToolDefinition:
    """Definition of a Tool including its Ollama compatible JSON schema."""

    name: str
    description: str
    func: ToolFunction
    schema: dict[str, Any]
    requires_confirmation: bool = False


class ToolRegistry:
    """Holds all registered tools and converts python funcs to LLM schemas."""

    def __init__(self) -> None:
        self._tools: dict[str, ToolDefinition] = {}

    def register(
        self,
        name: str,
        description: str,
        requires_confirmation: bool = False,
    ) -> Callable[[ToolFunction], ToolFunction]:
        """Decorator to register a function as a tool."""

        def decorator(func: ToolFunction) -> ToolFunction:
            schema = self._generate_schema(name, description, func)
            tool_def = ToolDefinition(
                name=name,
                description=description,
                func=func,
                schema=schema,
                requires_confirmation=requires_confirmation,
            )
            self._tools[name] = tool_def
            return func

        return decorator

    def get_tool(self, name: str) -> ToolDefinition | None:
        """Get a registered tool by name."""
        return self._tools.get(name)

    def get_schemas(self, tool_names: list[str]) -> list[dict[str, Any]]:
        """Get the Ollama JSON schemas for a list of tool names."""
        schemas = []
        for name in tool_names:
            if tool := self._tools.get(name):
                schemas.append(tool.schema)
        return schemas

    def _generate_schema(
        self, name: str, description: str, func: ToolFunction
    ) -> dict[str, Any]:
        """Inspect a python function and generate a JSON Schema for Ollama."""
        sig = inspect.signature(func)

        properties: dict[str, Any] = {}
        required: list[str] = []

        for param_name, param in sig.parameters.items():
            # Skip ctx/self etc if we ever add them, but for now simple funcs
            param_type = "string"  # Default fallback

            # Very basic type inference for MVP
            annotation = param.annotation
            # `is` rather than `==`: these are type objects, and a custom
            # annotation with an __eq__ override must not be mistaken for int.
            if annotation is int:
                param_type = "integer"
            elif annotation is bool:
                param_type = "boolean"
            elif annotation is float:
                param_type = "number"

            properties[param_name] = {
                "type": param_type,
            }

            if param.default == inspect.Parameter.empty:
                required.append(param_name)

        return {
            "type": "function",
            "function": {
                "name": name,
                "description": description,
                "parameters": {
                    "type": "object",
                    "properties": properties,
                    "required": required,
                },
            },
        }


# Global registry for convenience
registry = ToolRegistry()
