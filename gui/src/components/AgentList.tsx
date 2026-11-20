import type { Agent } from "../api";

interface Props {
  agents: Agent[];
  onPick?: (agent: Agent) => void;
  picked?: string;
}

export function AgentList({ agents, onPick, picked }: Props) {
  return (
    <div className="agent-list">
      <div className="agent-list-title">Agents</div>
      {agents.length === 0 && <div className="agent-empty">No agents loaded.</div>}
      {agents.map((agent) => (
        <div
          className={
            "agent-card" + (picked && picked === agent.name ? " picked" : "")
          }
          key={agent.name}
          onClick={onPick ? () => onPick(agent) : undefined}
          role={onPick ? "button" : undefined}
        >
          <div className="agent-name">{agent.name}</div>
          <div className="agent-desc">{agent.description}</div>
        </div>
      ))}
    </div>
  );
}
