import { useEffect, useState } from "react";
import {
  memoryStats,
  ollamaLoaded,
  statsBreakdown,
  systemStats,
  taskQueue,
  type Breakdown,
  type MemoryStats,
  type SystemStats,
  type TaskRow,
} from "../api";

/// The system dashboard (PRD 4.4): request/token/cost totals, the most
/// recent turns, live memory/CPU, loaded Ollama models and the background
/// task queue.
export function Dashboard() {
  const [stats, setStats] = useState<SystemStats | null>(null);
  const [mem, setMem] = useState<MemoryStats | null>(null);
  const [loaded, setLoaded] = useState<string[]>([]);
  const [tasks, setTasks] = useState<TaskRow[]>([]);
  const [breakdown, setBreakdown] = useState<Breakdown | null>(null);
  const [error, setError] = useState("");

  useEffect(() => {
    systemStats()
      .then(setStats)
      .catch((e) => setError(String(e)));
    statsBreakdown()
      .then(setBreakdown)
      .catch(() => {});
    const refresh = () => {
      memoryStats()
        .then(setMem)
        .catch(() => {});
      ollamaLoaded()
        .then(setLoaded)
        .catch(() => {});
      taskQueue()
        .then(setTasks)
        .catch(() => {});
    };
    refresh();
    const id = setInterval(refresh, 5000);
    return () => clearInterval(id);
  }, []);

  if (error) {
    return <div className="dashboard-error">⚠️ {error}</div>;
  }
  if (!stats) {
    return <div className="dashboard-empty">Loading…</div>;
  }

  return (
    <div className="dashboard">
      <div className="stats-cards">
        <div className="stat-card">
          <div className="stat-value">{stats.requests}</div>
          <div className="stat-label">Requests</div>
        </div>
        <div className="stat-card">
          <div className="stat-value">
            {stats.est_input_tokens.toLocaleString()}
          </div>
          <div className="stat-label">Est. input tokens</div>
        </div>
        <div className="stat-card">
          <div className="stat-value">${stats.cost_usd.toFixed(4)}</div>
          <div className="stat-label">Tracked cost</div>
        </div>
        {mem && (
          <div className="stat-card">
            <div className="stat-value">
              {mem.used_ram_gb.toFixed(1)}/{mem.total_ram_gb.toFixed(0)} GB
            </div>
            <div className="stat-label">RAM · CPU {Math.round(mem.cpu_percent)}%</div>
          </div>
        )}
      </div>

      <div className="dash-columns">
        <div className="dash-panel">
          <div className="panel-title">Loaded models (Ollama)</div>
          {loaded.length === 0 && (
            <div className="panel-empty">No models loaded.</div>
          )}
          {loaded.map((name) => (
            <div className="panel-row" key={name}>
              {name}
            </div>
          ))}
        </div>
        <div className="dash-panel">
          <div className="panel-title">Task queue</div>
          {tasks.length === 0 && <div className="panel-empty">Empty.</div>}
          {tasks.map((t) => (
            <div className="panel-row" key={t.id}>
              {t.status === "running" ? "▶" : "⏳"} {t.kind} {t.id} — {t.status}
            </div>
          ))}
        </div>
      </div>

      {breakdown && (
        <div className="dash-columns">
          <div className="dash-panel">
            <div className="panel-title">By day</div>
            {Object.entries(breakdown.per_day)
              .sort(([a], [b]) => (a < b ? 1 : -1))
              .slice(0, 14)
              .map(([day, b]) => (
                <div className="panel-row" key={day}>
                  {day}: {b.requests} req ·{" "}
                  {b.est_input_tokens.toLocaleString()} tok · $
                  {b.cost_usd.toFixed(4)}
                  {b.errors > 0 ? ` · ${b.errors} err` : ""}
                </div>
              ))}
          </div>
          <div className="dash-panel">
            <div className="panel-title">By model</div>
            {Object.entries(breakdown.per_model)
              .sort(([, a], [, b]) => b.requests - a.requests)
              .map(([model, b]) => (
                <div className="panel-row" key={model}>
                  {model}: {b.requests} req ·{" "}
                  {b.est_input_tokens.toLocaleString()} tok · $
                  {b.cost_usd.toFixed(4)}
                  {b.errors > 0 ? ` · ${b.errors} err` : ""}
                </div>
              ))}
          </div>
          <div className="dash-panel">
            <div className="panel-title">By agent</div>
            {Object.entries(breakdown.per_agent)
              .sort(([, a], [, b]) => b.requests - a.requests)
              .map(([agent, b]) => (
                <div className="panel-row" key={agent}>
                  {agent}: {b.requests} req ·{" "}
                  {b.est_input_tokens.toLocaleString()} tok · $
                  {b.cost_usd.toFixed(4)}
                  {b.errors > 0 ? ` · ${b.errors} err` : ""}
                </div>
              ))}
          </div>
        </div>
      )}

      <table className="requests-table">
        <thead>
          <tr>
            <th>Model</th>
            <th>Provider</th>
            <th>Tier</th>
            <th>Tokens</th>
            <th>Cost</th>
            <th>Duration</th>
            <th>Status</th>
          </tr>
        </thead>
        <tbody>
          {stats.recent.map((r, i) => (
            <tr key={i} className={r.error ? "row-error" : ""}>
              <td>{r.model}</td>
              <td>{r.provider}</td>
              <td>{r.tier ?? "—"}</td>
              <td>{r.est_input_tokens.toLocaleString()}</td>
              <td>{r.cost_usd != null ? `$${r.cost_usd.toFixed(4)}` : "—"}</td>
              <td>{Math.round(r.duration_ms)}ms</td>
              <td>{r.error ?? "ok"}</td>
            </tr>
          ))}
        </tbody>
      </table>
      {stats.recent.length === 0 && (
        <div className="dashboard-empty">No turns recorded yet.</div>
      )}
    </div>
  );
}
