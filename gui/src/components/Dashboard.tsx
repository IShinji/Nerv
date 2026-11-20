import { useEffect, useState } from "react";
import { systemStats, type SystemStats } from "../api";

/// The system dashboard: request/token/cost totals plus the most recent
/// turns, read from the trace log.
export function Dashboard() {
  const [stats, setStats] = useState<SystemStats | null>(null);
  const [error, setError] = useState("");

  useEffect(() => {
    systemStats()
      .then(setStats)
      .catch((e) => setError(String(e)));
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
      </div>
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
