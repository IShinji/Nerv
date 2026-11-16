//! Registry and core types for the tools the rust brain executes natively.
//! The python decorator registry derives schemas from signatures; the rust
//! registry declares them explicitly (each tool owns its schema).

use serde_json::{json, Value};

/// The result of executing a tool.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
    /// The tool was paused/suspended waiting for user approval.
    pub is_pending: bool,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            is_pending: false,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            is_pending: false,
        }
    }
}

/// Definition of a tool, including its Ollama-compatible JSON schema.
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub requires_confirmation: bool,
    pub parameters: Value,
    /// The native execution; async because tools may shell out.
    #[allow(clippy::type_complexity)]
    pub func: for<'a> fn(
        &'a ToolContext,
        &'a Value,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = ToolResult> + Send + 'a>,
    >,
}

/// What a tool implementation gets: the project root and the workspace guard.
pub struct ToolContext {
    pub project_root: PathBuf,
    pub workspace_root: PathBuf,
}

use std::path::PathBuf;

impl ToolDefinition {
    /// The Ollama-compatible function schema (the same shape the python
    /// registry's `_generate_schema` produced).
    pub fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            },
        })
    }
}

/// The registry of natively-executed tools.
pub struct ToolRegistry;

impl ToolRegistry {
    /// All registered tools, in declaration order.
    pub(crate) fn all() -> &'static [ToolDefinition] {
        static TOOLS: std::sync::LazyLock<Vec<ToolDefinition>> =
            std::sync::LazyLock::new(build_registry);
        &TOOLS
    }

    pub fn get_tool(&self, name: &str) -> Option<&ToolDefinition> {
        Self::all().iter().find(|t| t.name == name)
    }

    /// The Ollama JSON schemas for a list of tool names (deduped, order
    /// preserved — mirrors python `CapabilityRegistry.get_schemas`).
    pub fn get_schemas(&self, names: &[String]) -> Vec<Value> {
        let mut schemas: Vec<Value> = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        for name in names {
            if let Some(tool) = self.get_tool(name) {
                if !seen.contains(&tool.name) {
                    schemas.push(tool.schema());
                    seen.push(tool.name);
                }
            }
        }
        schemas
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self
    }
}

fn build_registry() -> Vec<ToolDefinition> {
    vec![
        super::system::current_time(),
        super::system::shell(),
        super::files::file_io(),
        super::files::read_file(),
        super::files::write_file_full(),
        super::files::grep_search(),
    ]
}
