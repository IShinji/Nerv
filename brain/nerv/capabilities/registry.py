"""Capability registry that resolves abstract capabilities to concrete tools."""

from __future__ import annotations

from dataclasses import dataclass

from nerv.tools.builtins import registry as tool_registry
from nerv.tools.registry import ToolDefinition, ToolRegistry


@dataclass(frozen=True)
class CapabilityDefinition:
    """An abstract capability backed by a concrete provider tool."""

    name: str
    provider_name: str
    provider_kind: str = "native"
    description: str = ""


@dataclass(frozen=True)
class CapabilityResolution:
    """A resolved capability or direct tool invocation."""

    requested_name: str
    provider_name: str
    provider_kind: str
    tool_def: ToolDefinition
    capability_name: str | None = None

    @property
    def is_capability(self) -> bool:
        """Return whether this resolution came from an abstract capability."""
        return self.capability_name is not None


class CapabilityRegistry:
    """Resolve abstract capability names to concrete tool providers."""

    def __init__(self, tools: ToolRegistry) -> None:
        self._tools = tools
        self._capabilities: dict[str, CapabilityDefinition] = {}

    def register(
        self,
        name: str,
        provider_name: str,
        description: str = "",
        provider_kind: str = "native",
    ) -> None:
        """Register a capability alias backed by a concrete provider tool."""
        self._capabilities[name] = CapabilityDefinition(
            name=name,
            provider_name=provider_name,
            provider_kind=provider_kind,
            description=description,
        )

    def resolve(self, name: str) -> CapabilityResolution | None:
        """Resolve either a direct tool name or a capability alias."""
        tool_def = self._tools.get_tool(name)
        if tool_def is not None:
            return CapabilityResolution(
                requested_name=name,
                provider_name=name,
                provider_kind="native",
                tool_def=tool_def,
            )

        capability = self._capabilities.get(name)
        if capability is None:
            return None

        tool_def = self._tools.get_tool(capability.provider_name)
        if tool_def is None:
            return None

        return CapabilityResolution(
            requested_name=name,
            provider_name=capability.provider_name,
            provider_kind=capability.provider_kind,
            tool_def=tool_def,
            capability_name=capability.name,
        )

    def resolve_step_target(self, identifier: str) -> tuple[CapabilityResolution | None, str]:
        """Resolve the callable target within a workflow step identifier."""
        if resolution := self.resolve(identifier):
            return resolution, ""

        parts = identifier.split(".")
        for idx in range(len(parts) - 1, 0, -1):
            candidate = ".".join(parts[:idx])
            method_hint = ".".join(parts[idx:])
            resolution = self.resolve(candidate)
            if resolution is not None:
                return resolution, method_hint

        return None, ""

    def get_schemas(self, names: list[str]) -> list[dict[str, object]]:
        """Return concrete tool schemas for a mix of tools and capabilities."""
        schemas: list[dict[str, object]] = []
        seen_provider_names: set[str] = set()

        for name in names:
            resolution = self.resolve(name)
            if resolution is None or resolution.provider_name in seen_provider_names:
                continue
            schemas.append(resolution.tool_def.schema)
            seen_provider_names.add(resolution.provider_name)

        return schemas

    def describe(self, names: list[str]) -> list[str]:
        """Render a user-facing list of requested capabilities/tools."""
        labels: list[str] = []
        for name in names:
            resolution = self.resolve(name)
            if resolution is None:
                continue
            if resolution.is_capability:
                labels.append(f"{name} (via {resolution.provider_name})")
            else:
                labels.append(name)
        return labels


capability_registry = CapabilityRegistry(tool_registry)

capability_registry.register(
    name="browser.read",
    provider_name="browser",
    description="Fetch and inspect a known webpage URL.",
)
capability_registry.register(
    name="browser.interactive",
    provider_name="chrome_browser",
    description="Operate an interactive browser session.",
)
capability_registry.register(
    name="filesystem.read",
    provider_name="file_io",
    description="Inspect files and directories.",
)
capability_registry.register(
    name="filesystem.read_exact",
    provider_name="read_file",
    description="Read the exact contents of a file.",
)
capability_registry.register(
    name="local.exec",
    provider_name="shell",
    description="Execute a local shell command.",
)
