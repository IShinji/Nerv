import { useEffect, useState } from "react";
import {
  hardwareProfile,
  ollamaModels,
  ollamaPull,
  saveApiKeys,
  saveChannels,
} from "../api";

interface Profile {
  os_name: string;
  arch: string;
  ram_gb: number;
  has_apple_silicon: boolean;
  recommended_router_model: string;
}

const API_KEY_FIELDS: [string, string][] = [
  ["ANTHROPIC_API_KEY", "Anthropic (claude models)"],
  ["OPENAI_API_KEY", "OpenAI (gpt / o1 models)"],
  ["GEMINI_API_KEY", "Google (gemini models)"],
  ["DEEPSEEK_API_KEY", "DeepSeek"],
];

/// The setup wizard (PRD Phase 3): hardware check → model recommendation
/// with one-click download → API keys → channels → done. Runs when no
/// agents are configured yet (first launch) and can be reopened from the
/// sidebar.
export function SetupWizard({ onDone }: { onDone: () => void }) {
  const [profile, setProfile] = useState<Profile | null>(null);
  const [models, setModels] = useState<string[]>([]);
  const [pulling, setPulling] = useState(false);
  const [status, setStatus] = useState("");
  const [error, setError] = useState("");

  // Step 3: API keys.
  const [keys, setKeys] = useState<Record<string, string>>({});
  const [keysSaved, setKeysSaved] = useState(false);

  // Step 4: channel enablement + credentials.
  const [telegramEnabled, setTelegramEnabled] = useState(false);
  const [telegramToken, setTelegramToken] = useState("");
  const [telegramUsers, setTelegramUsers] = useState("");
  const [emailEnabled, setEmailEnabled] = useState(false);
  const [emailHost, setEmailHost] = useState("");
  const [emailUser, setEmailUser] = useState("");
  const [emailPassword, setEmailPassword] = useState("");
  const [emailSender, setEmailSender] = useState("");
  const [slackEnabled, setSlackEnabled] = useState(false);
  const [slackAppToken, setSlackAppToken] = useState("");
  const [slackBotToken, setSlackBotToken] = useState("");
  const [slackUsers, setSlackUsers] = useState("");
  const [channelsSaved, setChannelsSaved] = useState(false);

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

  async function saveKeys() {
    await saveApiKeys(keys);
    setKeysSaved(true);
  }

  async function saveChannelConfig() {
    const channels: Record<string, unknown> = {
      cli: { enabled: true },
    };
    if (telegramEnabled) {
      channels.telegram = {
        enabled: true,
        bot_token: telegramToken,
        allowed_users: telegramUsers
          .split(",")
          .map((u) => u.trim())
          .filter(Boolean),
      };
    }
    if (emailEnabled) {
      channels.email = {
        enabled: true,
        imap_host: emailHost,
        smtp_host: emailHost.replace(/^imap\./, "smtp."),
        username: emailUser,
        password: emailPassword,
        allowed_senders: emailSender
          .split(",")
          .map((u) => u.trim())
          .filter(Boolean),
      };
    }
    if (slackEnabled) {
      channels.slack = {
        enabled: true,
        app_token: slackAppToken,
        bot_token: slackBotToken,
        allowed_users: slackUsers
          .split(",")
          .map((u) => u.trim())
          .filter(Boolean),
      };
    }
    await saveChannels(channels);
    setChannelsSaved(true);
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
        <div className="step-title">3. API keys (optional — for cloud models)</div>
        <div className="step-body">
          <div className="wizard-fields">
            {API_KEY_FIELDS.map(([key, label]) => (
              <label key={key} className="wizard-field">
                <span>{label}</span>
                <input
                  type="password"
                  value={keys[key] ?? ""}
                  placeholder={key}
                  onChange={(e) =>
                    setKeys((k) => ({ ...k, [key]: e.target.value }))
                  }
                />
              </label>
            ))}
          </div>
          <button onClick={() => void saveKeys()} disabled={keysSaved}>
            {keysSaved ? "✓ Saved" : "Save keys"}
          </button>
          <div className="wizard-hint">
            Stored in personal/api_keys.env (local only, mode 0600). Skip if
            you only use the claude-cli or local backends.
          </div>
        </div>
      </div>

      <div className="wizard-step">
        <div className="step-title">4. Channels (optional)</div>
        <div className="step-body">
          <label className="wizard-check">
            <input
              type="checkbox"
              checked={telegramEnabled}
              onChange={(e) => setTelegramEnabled(e.target.checked)}
            />
            Telegram
          </label>
          {telegramEnabled && (
            <div className="wizard-fields">
              <label className="wizard-field">
                <span>Bot token</span>
                <input
                  type="password"
                  value={telegramToken}
                  onChange={(e) => setTelegramToken(e.target.value)}
                />
              </label>
              <label className="wizard-field">
                <span>Allowed user ids (comma-separated)</span>
                <input
                  value={telegramUsers}
                  placeholder="123456789"
                  onChange={(e) => setTelegramUsers(e.target.value)}
                />
              </label>
            </div>
          )}
          <label className="wizard-check">
            <input
              type="checkbox"
              checked={emailEnabled}
              onChange={(e) => setEmailEnabled(e.target.checked)}
            />
            Email (IMAP/SMTP)
          </label>
          {emailEnabled && (
            <div className="wizard-fields">
              <label className="wizard-field">
                <span>IMAP host</span>
                <input
                  value={emailHost}
                  placeholder="imap.example.com"
                  onChange={(e) => setEmailHost(e.target.value)}
                />
              </label>
              <label className="wizard-field">
                <span>Username</span>
                <input
                  value={emailUser}
                  placeholder="nerv@example.com"
                  onChange={(e) => setEmailUser(e.target.value)}
                />
              </label>
              <label className="wizard-field">
                <span>Password</span>
                <input
                  type="password"
                  value={emailPassword}
                  onChange={(e) => setEmailPassword(e.target.value)}
                />
              </label>
              <label className="wizard-field">
                <span>Allowed senders (comma-separated)</span>
                <input
                  value={emailSender}
                  placeholder="you@example.com"
                  onChange={(e) => setEmailSender(e.target.value)}
                />
              </label>
            </div>
          )}
          <label className="wizard-check">
            <input
              type="checkbox"
              checked={slackEnabled}
              onChange={(e) => setSlackEnabled(e.target.checked)}
            />
            Slack (Socket Mode)
          </label>
          {slackEnabled && (
            <div className="wizard-fields">
              <label className="wizard-field">
                <span>App token (xapp-)</span>
                <input
                  type="password"
                  value={slackAppToken}
                  onChange={(e) => setSlackAppToken(e.target.value)}
                />
              </label>
              <label className="wizard-field">
                <span>Bot token (xoxb-)</span>
                <input
                  type="password"
                  value={slackBotToken}
                  onChange={(e) => setSlackBotToken(e.target.value)}
                />
              </label>
              <label className="wizard-field">
                <span>Allowed user ids (comma-separated)</span>
                <input
                  value={slackUsers}
                  placeholder="U0123456789"
                  onChange={(e) => setSlackUsers(e.target.value)}
                />
              </label>
            </div>
          )}
          <div className="wizard-hint">
            Every channel fails closed: an empty allow-list authorizes nobody.
          </div>
          <button onClick={() => void saveChannelConfig()}>
            {channelsSaved ? "✓ Saved to config.yaml" : "Save channels"}
          </button>
        </div>
      </div>

      <div className="wizard-step">
        <div className="step-title">5. Done</div>
        <div className="step-body">
          You can start chatting now — channels can be enabled later by
          reopening Setup, and every channel fails closed until its
          allow-list is set.
          <button onClick={onDone}>Start using Nerv</button>
        </div>
      </div>
    </div>
  );
}
