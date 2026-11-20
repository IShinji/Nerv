import { useEffect, useState } from "react";
import { AgentEditor } from "./components/AgentEditor";
import { AgentList } from "./components/AgentList";
import { Chat } from "./components/Chat";
import { Dashboard } from "./components/Dashboard";
import { checkHeartbeat, listAgents, type Agent } from "./api";

type Tab = "chat" | "agents" | "dashboard";

export function App() {
  const [agents, setAgents] = useState<Agent[]>([]);
  const [notice, setNotice] = useState<string>("");
  const [tab, setTab] = useState<Tab>("chat");
  const [editing, setEditing] = useState<Agent | null>(null);

  function refreshAgents() {
    listAgents()
      .then(setAgents)
      .catch(() => setAgents([]));
  }

  useEffect(refreshAgents, []);

  // Poll the brain heartbeat for proactive reminders (self-evolution
  // nudges, calendar reminders, unfinished workflows).
  useEffect(() => {
    const id = setInterval(async () => {
      try {
        const msg = await checkHeartbeat();
        if (msg) setNotice(msg);
      } catch {
        /* brain not ready yet */
      }
    }, 30000);
    return () => clearInterval(id);
  }, []);

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="brand">🧠 Nerv</div>
        <AgentList
          agents={agents}
          onPick={(agent) => {
            setEditing(agent);
            setTab("agents");
          }}
        />
      </aside>
      <main className="main">
        <nav className="tabs">
          <button
            className={tab === "chat" ? "tab active" : "tab"}
            onClick={() => setTab("chat")}
          >
            Chat
          </button>
          <button
            className={tab === "agents" ? "tab active" : "tab"}
            onClick={() => setTab("agents")}
          >
            Agents
          </button>
          <button
            className={tab === "dashboard" ? "tab active" : "tab"}
            onClick={() => setTab("dashboard")}
          >
            Dashboard
          </button>
        </nav>
        {notice && (
          <div className="notice" onClick={() => setNotice("")}>
            {notice}
          </div>
        )}
        {tab === "chat" && <Chat />}
        {tab === "agents" && (
          <div className="agents-page">
            <AgentList
              agents={agents}
              onPick={(agent) => setEditing(agent)}
              picked={editing?.name}
            />
            <AgentEditor agent={editing} onSaved={refreshAgents} />
          </div>
        )}
        {tab === "dashboard" && <Dashboard />}
      </main>
    </div>
  );
}
