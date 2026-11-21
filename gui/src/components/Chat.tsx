import { useEffect, useRef, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { readDroppedFile, sendMessage, type FilePayload, type ReplyMeta } from "../api";

interface Attachment {
  name: string;
  content: string;
}

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

const QUICK_COMMANDS: string[] = [
  "What's on my calendar today?",
  "Summarize this week's conversations",
  "Check my unfinished workflows",
];

export function Chat() {
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [dragging, setDragging] = useState(false);
  const [streamText, setStreamText] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const endRef = useRef<HTMLDivElement>(null);

  // 流式推送: while a send is in flight, the backend emits full-text
  // snapshots as `turn-progress` events; show the latest as a live bubble.
  useEffect(() => {
    if (!busy) return;
    let unlisten: UnlistenFn | undefined;
    listen<string>("turn-progress", (event) => {
      setStreamText(event.payload);
    }).then((off) => {
      unlisten = off;
    });
    return () => {
      unlisten?.();
    };
  }, [busy]);

  // 文件拖拽发送 (PRD 4.1): dropped paths are read and attached to the
  // next message. Text files inline; binaries attach as path references.
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    getCurrentWebview()
      .onDragDropEvent((event) => {
        if (event.payload.type === "enter" || event.payload.type === "over") {
          setDragging(true);
        } else if (event.payload.type === "leave") {
          setDragging(false);
        } else if (event.payload.type === "drop") {
          setDragging(false);
          for (const path of event.payload.paths) {
            readDroppedFile(path)
              .then((file: FilePayload) => {
                if (file.text != null) {
                  setAttachments((a) => [
                    ...a,
                    { name: file.name, content: file.text as string },
                  ]);
                } else {
                  setAttachments((a) => [
                    ...a,
                    {
                      name: file.name,
                      content: `[binary file: ${file.name} (${file.size_bytes} bytes) — Nerv can read it with file_io at its path]`,
                    },
                  ]);
                }
              })
              .catch((e) => console.error("drop read failed:", e));
          }
        }
      })
      .then((off) => {
        unlisten = off;
      });
    return () => {
      unlisten?.();
    };
  }, []);

  async function send() {
    const text = input.trim();
    if ((!text && attachments.length === 0) || busy) return;
    let composed = text;
    if (attachments.length > 0) {
      const parts = attachments.map(
        (a, i) => `[Attachment ${i + 1}: ${a.name}]\n${a.content}`,
      );
      composed = [text, ...parts].filter(Boolean).join("\n\n");
      setAttachments([]);
    }
    setInput("");
    setMessages((m) => [...m, { role: "user", text: composed }]);
    setBusy(true);
    setStreamText(null);
    try {
      const result = await sendMessage(composed);
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
      setStreamText(null);
      setBusy(false);
      requestAnimationFrame(() =>
        endRef.current?.scrollIntoView({ behavior: "smooth" }),
      );
    }
  }

  function removeAttachment(index: number) {
    setAttachments((a) => a.filter((_, i) => i !== index));
  }

  return (
    <div className={"chat" + (dragging ? " drag-over" : "")}>
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
        {busy && streamText != null && (
          <div className="bubble assistant streaming">{streamText}</div>
        )}
        {busy && streamText == null && (
          <div className="bubble assistant typing">…</div>
        )}
        <div ref={endRef} />
      </div>
      {attachments.length > 0 && (
        <div className="attachments">
          {attachments.map((a, i) => (
            <span className="attachment-chip" key={i}>
              📄 {a.name}
              <button
                className="attachment-remove"
                onClick={() => removeAttachment(i)}
              >
                ×
              </button>
            </span>
          ))}
        </div>
      )}
      <div className="quick-commands">
        {QUICK_COMMANDS.map((cmd) => (
          <button
            key={cmd}
            className="quick-command"
            onClick={() => setInput(cmd)}
            disabled={busy}
          >
            {cmd}
          </button>
        ))}
      </div>
      <div className="composer">
        <textarea
          value={input}
          placeholder={dragging ? "Drop files here…" : "Message Nerv…"}
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
