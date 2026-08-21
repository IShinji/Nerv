"""Agent Factory module for dynamically creating missing agents."""

import logging
from pathlib import Path

from nerv.config import get_tier_models
from nerv.orchestrator.registry import AgentDefinition

logger = logging.getLogger(__name__)

FACTORY_TIMEOUT = 120.0

FACTORY_SYSTEM_PROMPT = """\
You are an expert AI persona designer and software architect.
Your ONLY job is to generate a comprehensive YAML configuration file for a new AI Agent.
The YAML must EXACTLY match the following structure, with NO markdown block formatting outside of the yaml content itself (though you should just output raw yaml).

Required YAML Structure:
name: <PascalCase version of the role, e.g., FrontendDeveloper>
description: <A one sentence description of their expertise>
system_prompt: |
  <A highly detailed system prompt outlining their persona, constraints, and instructions.
  Make sure to tell the agent exactly what tools they should use and how to behave.>
tools:
  - <tool_name_1_if_applicable>
  - <tool_name_2_if_applicable>

Available capabilities and built-in tools for them to use (only include if strictly necessary for their role):
- browser.read (preferred abstract capability for inspecting known webpages; currently backed by browser)
- browser.interactive (preferred abstract capability for interactive browser tasks; routed through MCP when configured and otherwise falls back to local chrome_browser)
- filesystem.read (preferred abstract capability for directory and file inspection; currently backed by file_io)
- filesystem.read_exact (preferred abstract capability for exact file reads; currently backed by read_file)
- local.exec (preferred abstract capability for local command execution; currently backed by shell)
- current_time
- calendar_add (add an event to the local calendar)
- calendar_list (list upcoming local calendar events)
- note_write (save a Markdown note)
- note_read (read a saved note)
- note_list (list saved notes)
- send_email (send email; requires a configured email MCP server)
- file_io
- read_file
- write_file_full
- list_workflows
- get_workflow
- propose_workflow
- list_skills
- get_skill
- list_review_queue
- web_search
- browser
- mcp_presets
- mcp_status
- chrome_browser
- screenshot
- desktop_control
- shell (HIGH RISK: use only for sysadmins, fullstack devs, or testing. Can execute any bash command.)
- grep_search
- delegate_task (Allows delegating to other specialized agents. Best for architects / project managers.)

CRITICAL INSTRUCTIONS:
1. Do not wrap the output in ```yaml ... ``` code blocks. Output ONLY raw YAML text.
2. The tools must be an array of strings. Leave empty [] if none are needed.
3. Keep the system_prompt focused on their specific domain.
4. Do not include global runtime rules like language-following or generic honesty boilerplate; those are injected separately at runtime.\
"""

class AgentFactory:
    """Dynamically generates YAML configurations for unknown Agent roles."""

    def __init__(self, project_root: Path) -> None:
        self.project_root = project_root
        self.agents_dir = project_root / "personal" / "agents"
        # Persona design benefits from a capable model — use the tier-2 model.
        self.model = get_tier_models(project_root)[2]

        self.agents_dir.mkdir(parents=True, exist_ok=True)

    async def create_agent(self, role_name: str, user_intent_hint: str) -> AgentDefinition | None:
        """Ask LLM to design an agent for `role_name` based on hint, validate, and save it."""
        logger.info("Factory is creating a new agent for role: '%s'", role_name)

        user_prompt = (
            f"Please generate the agent YAML for the role: '{role_name}'.\n"
            f"The user's original request that triggered this was: '{user_intent_hint}'.\n"
            f"Make the system prompt perfectly suited to handle this type of request and future similar requests."
        )

        try:
            yaml_content = await self._generate_yaml(user_prompt)
            # Clean up markdown if the LLM hallucinated it despite instructions
            yaml_content = self._clean_yaml(yaml_content)

            # Save it
            file_path = self.agents_dir / f"{role_name}.yaml"
            file_path.write_text(yaml_content, encoding="utf-8")

            # Use the registry parser to validate and return
            import yaml
            data = yaml.safe_load(yaml_content)

            return AgentDefinition(**data)

        except Exception as e:
            logger.error("Factory failed to create agent '%s': %s", role_name, e)
            return None

    def _clean_yaml(self, text: str) -> str:
        """Strip markdown code block fences if present."""
        text = text.strip()
        if text.startswith("```yaml"):
            text = text[7:]
        elif text.startswith("```"):
            text = text[3:]

        if text.endswith("```"):
            text = text[:-3]

        return text.strip()

    async def _generate_yaml(self, user_prompt: str) -> str:
        """Generate the agent YAML via the configured model backend."""
        from nerv.llm import complete

        messages = [
            {"role": "system", "content": FACTORY_SYSTEM_PROMPT},
            {"role": "user", "content": user_prompt},
        ]
        result = await complete(
            messages,
            model=self.model,
            temperature=0.4,  # Slightly creative for persona design
            project_root=self.project_root,
            timeout=FACTORY_TIMEOUT,
        )
        return result.content
