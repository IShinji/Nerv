import { useEffect, useState } from "react";
import { AgentEditor } from "./components/AgentEditor";
import { AgentList } from "./components/AgentList";
import { Chat } from "./components/Chat";
import { Dashboard } from "./components/Dashboard";
import { Settings } from "./components/Settings";
import { SetupWizard } from "./components/SetupWizard";
import { checkHeartbeat, listAgents, type Agent } from "./api";

type Tab = "chat" | "agents" | "dashboard" | "settings" | "setup";

export function App() {
  const [agents, setAgents] = useState<Agent[]>([]);
  const [notice, setNotice] = useState<string>("");
  const [tab, setTab] = useState<Tab>("chat");
  const [editing, setEditing] = useState<Agent | null>(null);
  const [creating, setCreating] = useState(false);
  // First launch: no agents configured at all → open the wizard.
  const [firstLaunch, setFirstLaunch] = useState(false);

  function refreshAgents() {
    listAgents()
      .then((loaded) => {
        setAgents(loaded);
        if (loaded.length === 0) {
          setFirstLaunch(true);
        }
      })
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

  // First launch lands on the wizard instead of an empty chat.
  if (firstLaunch && tab === "chat") {
    setTab("setup");
  }

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
            className={tab === "setup" ? "tab active" : "tab"}
            onClick={() => setTab("setup")}
          >
            Setup
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
          <button
            className={tab === "settings" ? "tab active" : "tab"}
            onClick={() => setTab("settings")}
          >
            Settings
          </button>
        </nav>
        {notice && (
          <div className="notice" onClick={() => setNotice("")}>
            {notice}
          </div>
        )}
        {tab === "chat" && <Chat />}
        {tab === "setup" && <SetupWizard onDone={() => setTab("chat")} />}
        {tab === "agents" && (
          <div className="agents-page">
            <AgentList
              agents={agents}
              onPick={(agent) => {
                setEditing(agent);
                setCreating(false);
              }}
              picked={editing?.name}
            />
            <div className="agents-toolbar">
              <button
                className={creating ? "tab active" : "tab"}
                onClick={() => {
                  setCreating(!creating);
                  setEditing(null);
                }}
              >
                + New agent
              </button>
            </div>
            <AgentEditor
              agent={creating ? null : editing}
              creating={creating}
              onSaved={() => {
                refreshAgents();
                setCreating(false);
              }}
              onCancelNew={() => setCreating(false)}
            />
          </div>
        )}
        {tab === "dashboard" && <Dashboard />}
        {tab === "settings" && <Settings />}
      </main>
    </div>
  );
}
