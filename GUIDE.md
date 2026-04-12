# Nerv System Guide

Welcome to the Nerv Personal AI Operating System! This guide helps you understand how to interact with your agents, create reliable automations, and extend the system.

## 1. Chatting with Agents

By default, you talk to the system via your chosen channel (e.g., Telegram). The **Router** (a lightweight local model) categorizes your message and assigns it to the most relevant **Agent** (General, Coder, Researcher, etc.).

- **Simple Questions**: Will be handled quickly via `web_search` or local knowledge.
- **Complex Tasks**: Will trigger the agent to break down your request and use tools (e.g., writing tests, scanning directories).
- **Missing Experts**: If no existing agent matches your intent, the **Agent Factory** will automatically create a new expert YAML file for you, which you can keep forever!

## 2. The Automation Engine (Skills vs. Workflows)

Nerv uses a **Dual-Track** system to make automation reliable while avoiding AI hallucinations.

### Skills (The "How-To")
- **What they are**: Markdown files with behavioral rules.
- **Use case**: When you want an agent to learn a subjective judging criteria or a preferred structure (e.g., "How I like my code formatted" or "How to critique a resume").
- **How to create**: Put a `SKILL.md` (and a YAML metadata header) into the `skills/` library. The system will inject this into the agent's context when needed.

### Workflows (The "推土机 / Bulldozer")
- **What they are**: Strict step-by-step YAML scripts natively executed by the `WorkflowExecutor`.
- **Use case**: When you have a massive multi-step operational task (like clicking around a UI, waiting for elements, scraping text). You do NOT want an LLM guessing what to click next or hallucinating an intermediate state.
- **How to create**: Create a `.yaml` file in the `workflows/` directory.

Example Workflow step:
```yaml
steps:
  - "chrome_browser.open_url -> https://example.com"
  - "chrome_browser.click_element -> #submitButton"
```
The Native Executor safely runs this without relying on LLM logic loops.

## 3. Tool Parsimony (Smart Fallback)

You do **not** need to manually instruct agents which tool to use. 
Nerv naturally follows the **Tool Parsimony Rule**:
> The AI prefers cheap, blindingly fast API calls (like `web_search`) over heavy, slow GUI automation workflows (like `chrome_browser`).

If you just ask "what's the weather today", Nerv will use a quick web search. It won't pop open Google Chrome or fire up heavy automation unless you explicitly instruct it to (e.g., "Use my Chrome Gemini Workflow to get the weather").

## 4. Reviewing Proposals

When agents invent complex new workflows, they might stage them in the **Review Queue** (`reviews/workflows/`). 
In your chat (e.g. Telegram), you might see:
> "Workflow proposal queued for review: X. Please reply 'approve workflow X' or 'reject workflow X'."

Type those exact trigger commands inside the chat to approve them!

## 5. Development Mode

If you are developing Nerv locally, you can use the built-in `CLIAdapter`:
```bash
cd core/gateway
cargo run
```
You can type messages directly into your terminal, and all verbose system logs (such as the step-by-step progress of the `WorkflowExecutor`) will be cleanly printed to your terminal's `stderr` in real-time.
