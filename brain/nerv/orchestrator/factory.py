"""Agent Factory module for dynamically creating missing agents."""

import logging
import os
import re
from pathlib import Path
from typing import Any

import httpx

from nerv.orchestrator.registry import AgentDefinition

logger = logging.getLogger(__name__)

DEFAULT_OLLAMA_URL = "http://localhost:11434"
OLLAMA_TIMEOUT = 60.0

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
- browser.interactive (preferred abstract capability for interactive browser tasks; currently backed by chrome_browser and intended to map to MCP/browser providers over time)
- filesystem.read (preferred abstract capability for directory and file inspection; currently backed by file_io)
- filesystem.read_exact (preferred abstract capability for exact file reads; currently backed by read_file)
- local.exec (preferred abstract capability for local command execution; currently backed by shell)
- current_time
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
        self.base_url = os.environ.get("NERV_OLLAMA_URL", DEFAULT_OLLAMA_URL)
        self.model = os.environ.get("NERV_ROUTER_MODEL", "qwen2.5:1.5b") # Fast model to generate
        self._client = httpx.AsyncClient(timeout=OLLAMA_TIMEOUT)
        
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
            yaml_content = await self._call_ollama(user_prompt)
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

    async def _call_ollama(self, user_prompt: str) -> str:
        """Call Ollama HTTP API for chat completion."""
        url = f"{self.base_url}/api/chat"
        payload = {
            "model": self.model,
            "messages": [
                {"role": "system", "content": FACTORY_SYSTEM_PROMPT},
                {"role": "user", "content": user_prompt},
            ],
            "stream": False,
            "options": {
                "temperature": 0.4, # Slightly creative for persona design
            },
        }

        response = await self._client.post(url, json=payload)
        response.raise_for_status()

        data = response.json()
        content = data.get("message", {}).get("content", "")
        return content
