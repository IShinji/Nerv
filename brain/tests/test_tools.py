"""Tests for Tool Layer."""

from nerv.tools.registry import registry
import nerv.tools.builtins  # Ensure builtins are registered

def test_registry_schemas_generation() -> None:
    """Test tools are registered correctly."""
    schemas = registry.get_schemas(["read_file", "write_file_full"])
    assert len(schemas) == 2
    
    # Check read_file schema
    read_schema = schemas[0]
    assert read_schema["type"] == "function"
    assert read_schema["function"]["name"] == "read_file"
    assert "absolute_path" in read_schema["function"]["parameters"]["properties"]
    
def test_greedy_tools() -> None:
    """Test builtin tools behavior without executing side effects too badly."""
    tool = registry.get_tool("shell")
    assert tool is not None
    assert tool.requires_confirmation is True
    
    tool = registry.get_tool("read_file")
    assert tool is not None
    assert tool.requires_confirmation is False
