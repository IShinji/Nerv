# Nerv — Code Conventions

> 本文件是项目唯一的代码规范来源。所有贡献者（人类和AI）都必须遵守。
> AI 工具配置文件（CLAUDE.md, AGENTS.md, .cursorrules）引用本文件。

---

## 1. 语言与编码

- 代码、注释、变量名、函数名：**全部英文**
- 文档（README、用户面文档）：中英双语或英文
- Commit message：英文
- 字符编码：UTF-8，LF 换行（不用 CRLF）

## 2. 项目结构

```
nerv/
├── core/              # Rust — Gateway, hardware detection, process mgmt
│   ├── Cargo.toml     # Cargo workspace root
│   ├── gateway/       # Channel adapters, message routing
│   ├── detector/      # Hardware detection
│   └── shared/        # Shared types, config, IPC protocol
├── brain/             # Python — Router, orchestrator, agents, memory
│   ├── pyproject.toml # Project config (use uv or poetry)
│   ├── nerv/          # Main package
│   │   ├── router/
│   │   ├── orchestrator/
│   │   ├── agents/
│   │   ├── context/
│   │   ├── memory/
│   │   └── tools/
│   └── tests/
├── agents/            # Agent definitions (YAML)
├── docs/              # Public technical docs
├── gui/               # Tauri + React (Phase 2)
└── tests/             # Integration tests
```

## 3. Rust 规范

### 风格
- 使用 `rustfmt` 默认配置，不自定义
- 使用 `clippy` 并修复所有警告
- Edition: 2021+
- 缩进: 4 spaces

### 命名
```rust
// 类型、Trait: PascalCase
struct ChannelAdapter;
trait MessageHandler;
enum ChannelType;

// 函数、变量: snake_case
fn send_message() {}
let user_id = "abc";

// 常量: SCREAMING_SNAKE_CASE
const MAX_RETRY_COUNT: u32 = 3;

// 模块、文件: snake_case
mod telegram_adapter;
```

### 错误处理
- 库代码用 `thiserror` 定义错误类型
- 应用代码用 `anyhow` 处理错误
- 不用 `.unwrap()`，除非在测试中或有明确注释说明为什么安全
- 用 `?` 传播错误，不用 `.expect()` 做流程控制

### 异步
- Runtime: `tokio`
- Channel 通信: `tokio::sync::mpsc`
- 所有 I/O 操作用 async

### 依赖管理
- 最小依赖原则：能用标准库解决就不加 crate
- 新增依赖需在 PR 中说明原因
- 锁定版本：提交 `Cargo.lock`

## 4. Python 规范

### 风格
- Formatter: `ruff format`（兼容 black 风格）
- Linter: `ruff check`
- Type checker: 所有公开函数必须有 type hints
- 缩进: 4 spaces
- 行宽: 88 字符（ruff 默认）

### 命名
```python
# 类: PascalCase
class AgentFactory:
    pass

# 函数、变量: snake_case
def create_agent():
    user_name = "abc"

# 常量: SCREAMING_SNAKE_CASE
MAX_CONTEXT_TOKENS = 4000

# 私有: 前缀下划线
def _internal_helper():
    pass

# 模块、文件: snake_case
# router.py, agent_factory.py
```

### 类型标注
```python
# 所有公开函数必须标注参数和返回类型
def classify_intent(message: str, context: dict[str, Any] | None = None) -> RouterResult:
    ...

# 数据结构用 dataclass 或 Pydantic BaseModel
@dataclass
class RouterResult:
    intent: str
    complexity: str
    model_tier: int
    agent_type: str
```

### 异步
- 使用 `asyncio`
- I/O 密集操作用 async
- CPU 密集操作用 `concurrent.futures`

### 依赖管理
- 使用 `pyproject.toml`（不用 requirements.txt）
- 锁文件：提交 lock file
- 虚拟环境：使用 `uv` 或 `poetry`

## 5. Rust ↔ Python IPC 协议

通信格式：JSON-RPC 2.0 over stdin/stdout

```json
// Request (Rust → Python)
{"jsonrpc": "2.0", "method": "route", "params": {"message": "help me write code"}, "id": 1}

// Response (Python → Rust)
{"jsonrpc": "2.0", "result": {"intent": "code_generation", "model_tier": 1, "agent_type": "coder"}, "id": 1}

// Error
{"jsonrpc": "2.0", "error": {"code": -32000, "message": "Model unavailable"}, "id": 1}
```

- 每条消息占一行（newline-delimited JSON）
- stderr 保留给日志输出，不用于 IPC
- Python 进程由 Rust 启动和管理（sidecar 模式）

## 6. 配置文件规范

- 格式：YAML
- 位置：`~/.nerv/config.yaml`（用户配置）覆盖 `core/config.default.yaml`（默认配置）
- 所有配置项必须有默认值，用户配置文件可以只覆盖部分

```yaml
# 用 snake_case 作为 key
# 用注释说明每个配置项的作用
channels:
  telegram:
    enabled: true
    bot_token: ""       # Telegram Bot API token

models:
  router:
    provider: "ollama"  # ollama | anthropic | openai | google
    model: "qwen2.5:3b"
```

## 7. Agent 定义规范

- 格式：YAML
- 位置：`agents/` (预设) 或 `~/.nerv/personal/agents/` (用户)
- 文件名：`{snake_case_name}.yaml`

必填字段：
```yaml
name: "TaxAdvisor"              # PascalCase，唯一标识
description: "..."              # 一句话描述
tags: ["tax", "finance"]        # 用于模糊匹配
model_tier: [1, 2]              # 支持的模型等级
system_prompt: |                # Agent 的 system prompt
  ...
tools: []                       # 可用工具列表
```

## 8. Git 规范

### Branch 命名
```
main              # 稳定版本
dev               # 开发分支
feat/xxx          # 新功能
fix/xxx           # Bug 修复
refactor/xxx      # 重构
docs/xxx          # 文档
```

### Commit Message
格式：`<type>: <description>`

```
feat: add telegram channel adapter
fix: handle empty response from ollama
refactor: extract message parsing into shared crate
docs: update architecture diagram
chore: update dependencies
test: add router classification tests
```

- 用英文
- 首字母小写
- 不加句号
- 描述做了什么，不描述为什么（why 放在 PR description 里）

### PR 规范
- 标题同 commit message 格式
- Description 中说明：做了什么、为什么、怎么测试
- 有代码改动必须有对应测试

## 9. 测试规范

### Rust
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_function_name_describes_behavior() {
        // Arrange
        // Act
        // Assert
    }

    #[tokio::test]
    async fn test_async_operations() {
        // ...
    }
}
```

### Python
```python
# tests/ 目录结构映射 nerv/ 包结构
# 文件名: test_xxx.py
# 函数名: test_xxx_describes_behavior

def test_router_classifies_code_request():
    # Arrange
    # Act
    # Assert
    pass

@pytest.mark.asyncio
async def test_agent_factory_creates_new_agent():
    # ...
    pass
```

- 测试函数名描述行为，不描述实现
- 使用 Arrange-Act-Assert 结构
- Mock 外部依赖（Ollama API、Telegram API）

## 10. 安全规范

- API keys 永远不进代码仓库（用 `.env` + `.gitignore`）
- 用户数据目录 (`~/.nerv/personal/`) 不进仓库
- Shell 工具执行用户命令时必须有超时和沙箱
- 日志中不打印 API keys、用户消息的完整内容

## 11. 注释规范

```rust
// 单行注释：解释 WHY，不解释 WHAT
// 好的代码本身说明 what，注释说明 why

/// 文档注释：用于公开 API
/// 包含：简述、参数说明、返回值、示例
```

```python
# 同上原则：注释说明 why，不说明 what

def classify_intent(message: str) -> RouterResult:
    """Classify user message intent using local model.

    Args:
        message: Raw user message text.

    Returns:
        RouterResult with intent, complexity, and suggested model tier.
    """
```

- 不写无意义注释（`# increment counter` `# return result`）
- TODO 格式：`// TODO(username): description`
