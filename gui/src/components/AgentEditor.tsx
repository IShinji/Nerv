import { useEffect, useState } from "react";
import { getAgent, saveAgent, type Agent } from "../api";

interface Props {
  agent: Agent | null;
  onSaved: () => void;
}

/// The agent editor: load an agent's YAML, edit it, save into
/// personal/agents (the registry hot-loads personal on every turn).
export function AgentEditor({ agent, onSaved }: Props) {
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

  if (!agent) {
    return <div className="editor-empty">Select an agent to edit it.</div>;
  }

  const current: Agent = agent;

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
