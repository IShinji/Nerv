import { useEffect, useState } from "react";
import { hardwareProfile, ollamaModels, ollamaPull } from "../api";

interface Profile {
  os_name: string;
  arch: string;
  ram_gb: number;
  has_apple_silicon: boolean;
  recommended_router_model: string;
}

/// The setup wizard (PRD Phase 3): hardware check → model recommendation
/// with one-click download → done. Runs when no agents are configured yet
/// (first launch) and can be reopened from the sidebar.
export function SetupWizard({ onDone }: { onDone: () => void }) {
  const [profile, setProfile] = useState<Profile | null>(null);
  const [models, setModels] = useState<string[]>([]);
  const [pulling, setPulling] = useState(false);
  const [status, setStatus] = useState("");
  const [error, setError] = useState("");

  useEffect(() => {
    hardwareProfile()
      .then(setProfile)
      .catch((e: unknown) => setError(String(e)));
    ollamaModels()
      .then(setModels)
      .catch(() => setModels([]));
  }, []);

  if (error) {
    return <div className="wizard-error">⚠️ {error}</div>;
  }
  if (!profile) {
    return <div className="wizard-empty">Detecting hardware…</div>;
  }

  const recommended = profile.recommended_router_model;
  const installed = models.includes(recommended);

  async function pull() {
    setPulling(true);
    setStatus(`Downloading ${recommended}… this can take a few minutes.`);
    try {
      await ollamaPull(recommended);
      setModels((m) => [...m, recommended]);
      setStatus(`${recommended} is ready.`);
    } catch (e) {
      setStatus(`⚠️ ${String(e)}`);
    } finally {
      setPulling(false);
    }
  }

  return (
    <div className="wizard">
      <div className="wizard-title">Setup</div>

      <div className="wizard-step">
        <div className="step-title">1. Hardware</div>
        <div className="step-body">
          {profile.os_name} ({profile.arch}) · {profile.ram_gb} GB RAM
          {profile.has_apple_silicon ? " · Apple Silicon" : ""}
        </div>
      </div>

      <div className="wizard-step">
        <div className="step-title">2. Recommended model</div>
        <div className="step-body">
          <span className="model-name">{recommended}</span>
          {installed ? (
            <span className="model-ok">✓ installed</span>
          ) : (
            <button onClick={pull} disabled={pulling}>
              {pulling ? "Downloading…" : "Download"}
            </button>
          )}
          <div className="wizard-status">{status}</div>
        </div>
      </div>

      <div className="wizard-step">
        <div className="step-title">3. Done</div>
        <div className="step-body">
          {installed ? (
            <>
              Your router model is ready. Configure channels in
              <code> config.yaml</code> (Telegram, Email, Slack — each fails
              closed until its allow-list is set), then start chatting.
              <button onClick={onDone}>Start using Nerv</button>
            </>
          ) : (
            <>Download the model above to finish setup.</>
          )}
        </div>
      </div>
    </div>
  );
}
