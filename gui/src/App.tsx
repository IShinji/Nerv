import { useEffect, useState } from "react";
import { AgentList } from "./components/AgentList";
import { Chat } from "./components/Chat";
import { checkHeartbeat, listAgents, type Agent } from "./api";

export function App() {
  const [agents, setAgents] = useState<Agent[]>([]);
  const [notice, setNotice] = useState<string>("");

  useEffect(() => {
    listAgents()
      .then(setAgents)
      .catch(() => setAgents([]));
  }, []);

  // Poll the brain heartbeat for proactive reminders (M5).
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
        <AgentList agents={agents} />
      </aside>
      <main className="main">
        {notice && (
          <div className="notice" onClick={() => setNotice("")}>
            {notice}
          </div>
        )}
        <Chat />
      </main>
    </div>
  );
}
