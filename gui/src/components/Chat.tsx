import { useRef, useState } from "react";
import { sendMessage, type ReplyMeta } from "../api";

interface Message {
  role: "user" | "assistant";
  text: string;
  meta?: ReplyMeta;
}

/// Human-readable per-message annotation (PRD 4.1): model, tokens, cost.
function annotate(meta: ReplyMeta | undefined): string | null {
  if (!meta?.model) return null;
  const bits: string[] = [meta.model];
  if (meta.est_input_tokens) {
    bits.push(`${meta.est_input_tokens.toLocaleString()} tok`);
  }
  if (meta.cost_usd != null && meta.cost_usd > 0) {
    bits.push(`$${meta.cost_usd.toFixed(4)}`);
  }
  if (meta.duration_ms) {
    bits.push(`${(meta.duration_ms / 1000).toFixed(1)}s`);
  }
  return bits.join(" · ");
}

export function Chat() {
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const endRef = useRef<HTMLDivElement>(null);

  async function send() {
    const text = input.trim();
    if (!text || busy) return;
    setInput("");
    setMessages((m) => [...m, { role: "user", text }]);
    setBusy(true);
    try {
      const result = await sendMessage(text);
      setMessages((m) => [
        ...m,
        { role: "assistant", text: result.reply, meta: result },
      ]);
    } catch (e) {
      setMessages((m) => [
        ...m,
        { role: "assistant", text: `⚠️ ${String(e)}` },
      ]);
    } finally {
      setBusy(false);
      requestAnimationFrame(() =>
        endRef.current?.scrollIntoView({ behavior: "smooth" }),
      );
    }
  }

  return (
    <div className="chat">
      <div className="messages">
        {messages.length === 0 && (
          <div className="placeholder">Ask Nerv anything…</div>
        )}
        {messages.map((m, i) => {
          const annotation = m.role === "assistant" ? annotate(m.meta) : null;
          return (
            <div key={i} className={`bubble ${m.role}`}>
              {m.text}
              {annotation && <div className="bubble-meta">{annotation}</div>}
            </div>
          );
        })}
        {busy && <div className="bubble assistant typing">…</div>}
        <div ref={endRef} />
      </div>
      <div className="composer">
        <textarea
          value={input}
          placeholder="Message Nerv…"
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              void send();
            }
          }}
        />
        <button onClick={() => void send()} disabled={busy}>
          Send
        </button>
      </div>
    </div>
  );
}
