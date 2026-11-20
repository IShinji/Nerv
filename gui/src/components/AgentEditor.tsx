import { useEffect, useState } from "react";
import { getAgent, saveAgent, type Agent } from "../api";

interface Props {
  agent: Agent | null;
  creating?: boolean;
  onSaved: () => void;
  onCancelNew: () => void;
}

const NEW_AGENT_TEMPLATE = `name: NewAgent
description: (one sentence describing the expertise)
system_prompt: |
  You are a specialist agent. Describe your domain, constraints and behavior here.
tools: []
model_tier: [0]
`;

/// The agent editor: load an agent's YAML, edit it, save into
/// personal/agents (the registry hot-loads personal on every turn).
export function AgentEditor({ agent, creating, onSaved, onCancelNew }: Props) {
  const [yaml, setYaml] = useState("");
  const [status, setStatus] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (!agent) {
      setYaml("");
      return;
    }
    getAgent(agent.name)
      .then(setYaml)
      .catch((e) => setYaml(`# Failed to load: ${String(e)}`));
    setStatus("");
  }, [agent]);

  // A null agent with `creating` mode shows the template editor.
  if (!agent) {
    if (!creating) {
      return <div className="editor-empty">Select an agent to edit it.</div>;
    }
    return (
      <div className="agent-editor">
        <div className="editor-title">New agent</div>
        <textarea
          className="editor-yaml"
          value={yaml || NEW_AGENT_TEMPLATE}
          spellCheck={false}
          onChange={(e) => setYaml(e.target.value)}
        />
        <div className="editor-actions">
          <button onClick={() => void saveNew()} disabled={yaml === ""}>
            Create
          </button>
          <button className="editor-cancel" onClick={onCancelNew}>
            Cancel
          </button>
          <span className="editor-status">{status}</span>
        </div>
      </div>
    );
  }

  const current: Agent = agent;

  async function saveNew() {
    setBusy(true);
    setStatus("");
    try {
      // The template's `name:` field decides the file name.
      const match = yaml.match(/^name:\s*(.+)$/m);
      const name = match ? match[1].trim() : "";
      if (!name) {
        setStatus("⚠️ The YAML must include a `name:` field.");
        setBusy(false);
        return;
      }
      await saveAgent(name, yaml);
      setStatus("Created.");
      onSaved();
    } catch (e) {
      setStatus(`⚠️ ${String(e)}`);
    } finally {
      setBusy(false);
    }
  }

  async function save() {
    setBusy(true);
    setStatus("");
    try {
      await saveAgent(current.name, yaml);
      setStatus("Saved.");
      onSaved();
    } catch (e) {
      setStatus(`⚠️ ${String(e)}`);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="agent-editor">
      <div className="editor-title">{agent.name}</div>
      <textarea
        className="editor-yaml"
        value={yaml}
        spellCheck={false}
        onChange={(e) => setYaml(e.target.value)}
      />
      <div className="editor-actions">
        <button onClick={save} disabled={busy || yaml === ""}>
          {busy ? "Saving…" : "Save"}
        </button>
        <span className="editor-status">{status}</span>
      </div>
    </div>
  );
}
