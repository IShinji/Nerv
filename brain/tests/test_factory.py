from unittest.mock import AsyncMock, patch

import pytest

from nerv.orchestrator.factory import AgentFactory


@pytest.fixture
def temp_project_root(tmp_path):
    # Setup mock structure
    (tmp_path / "personal" / "agents").mkdir(parents=True)
    return tmp_path


@pytest.mark.asyncio
async def test_agent_factory_success(temp_project_root):
    factory = AgentFactory(temp_project_root)

    mock_yaml = """
name: DatabaseAdmin
description: Expert at optimizing queries and indexing.
system_prompt: |
  You are a very talented DBA.
  You understand MySQL and Postgres.
tools:
  - shell
"""

    with patch.object(factory, "_generate_yaml", new_callable=AsyncMock) as mock_call:
        mock_call.return_value = mock_yaml

        agent = await factory.create_agent(
            "database_admin", "I want to optimize my postgres queries"
        )

        assert agent is not None
        assert agent.name == "DatabaseAdmin"
        assert agent.description.startswith("Expert at optimizing")
        assert "shell" in agent.tools

        # Verify it was saved
        saved_file = temp_project_root / "personal" / "agents" / "database_admin.yaml"
        assert saved_file.exists()
        assert "name: DatabaseAdmin" in saved_file.read_text()


@pytest.mark.asyncio
async def test_agent_factory_cleans_markdown(temp_project_root):
    factory = AgentFactory(temp_project_root)

    mock_yaml = """```yaml
name: LegalAdvisor
description: Expert legal counsel.
system_prompt: Provides legal advice.
tools: []
```"""

    with patch.object(factory, "_generate_yaml", new_callable=AsyncMock) as mock_call:
        mock_call.return_value = mock_yaml

        agent = await factory.create_agent(
            "legal_advisor", "I need to review a contract"
        )
        assert agent is not None
        assert agent.name == "LegalAdvisor"
        assert len(agent.tools) == 0
