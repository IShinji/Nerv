import type { Agent } from "../api";

export function AgentList({ agents }: { agents: Agent[] }) {
  return (
    <div className="agent-list">
      <div className="agent-list-title">Agents</div>
      {agents.length === 0 && <div className="agent-empty">No agents loaded.</div>}
      {agents.map((agent) => (
        <div className="agent-card" key={agent.name}>
          <div className="agent-name">{agent.name}</div>
          <div className="agent-desc">{agent.description}</div>
        </div>
      ))}
    </div>
  );
}
