import { useState, useRef, useEffect, useCallback } from "react";
import { useQuery } from "@tanstack/react-query";
import { getToken, apiPath } from "../lib/api";
import { Button } from "../components/ui/button";
import { Input } from "../components/ui/input";

interface Model {
  id: string;
  object: string;
  created: number;
  owned_by: string;
}

interface Message {
  role: "user" | "assistant";
  content: string;
}

export function Playground() {
  const [model, setModel] = useState("");
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [streaming, setStreaming] = useState(false);
  const [error, setError] = useState("");
  const bottomRef = useRef<HTMLDivElement>(null);

  const { data: modelList } = useQuery({
    queryKey: ["models"],
    queryFn: async () => {
      const token = getToken();
      const headers: Record<string, string> = {
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      };
      const res = await fetch(apiPath("/v1/models"), { headers });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const json = await res.json();
      return (json.data || []) as Model[];
    },
  });

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  useEffect(() => {
    if (modelList && modelList.length > 0 && !model) {
      setModel(modelList[0].id);
    }
  }, [modelList, model]);

  const send = useCallback(async () => {
    const text = input.trim();
    if (!text || streaming) return;

    setError("");
    setInput("");
    setStreaming(true);

    const userMsg: Message = { role: "user", content: text };
    const updated = [...messages, userMsg];
    setMessages(updated);

    try {
      const token = getToken();
      const headers: Record<string, string> = {
        "Content-Type": "application/json",
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
      };

      const res = await fetch(apiPath("/v1/chat/completions"), {
        method: "POST",
        headers,
        body: JSON.stringify({
          model,
          messages: updated.map((m) => ({
            role: m.role,
            content: m.content,
          })),
          stream: true,
        }),
      });

      if (!res.ok) {
        const body = await res.text();
        throw new Error(`HTTP ${res.status}: ${body}`);
      }

      const reader = res.body?.getReader();
      if (!reader) throw new Error("No response body");

      const decoder = new TextDecoder();
      let assistantContent = "";

      setMessages((prev) => [...prev, { role: "assistant", content: "" }]);

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        const chunk = decoder.decode(value, { stream: true });
        const lines = chunk.split("\n");

        for (const line of lines) {
          if (!line.startsWith("data: ")) continue;
          const data = line.slice(6).trim();
          if (data === "[DONE]") continue;

          try {
            const parsed = JSON.parse(data);
            const delta = parsed.choices?.[0]?.delta?.content;
            if (delta) {
              assistantContent += delta;
              setMessages((prev) => {
                const copy = [...prev];
                copy[copy.length - 1] = {
                  role: "assistant",
                  content: assistantContent,
                };
                return copy;
              });
            }
          } catch {
            // skip unparseable chunks
          }
        }
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Chat request failed");
    } finally {
      setStreaming(false);
    }
  }, [input, streaming, messages, model]);

  return (
    <div className="flex h-[calc(100vh-6rem)] flex-col">
      <div className="mb-4 flex items-center justify-between">
        <h2 className="text-xl font-semibold">Chat Playground</h2>
        <select
          value={model}
          onChange={(e) => setModel(e.target.value)}
          className="rounded-md border border-border bg-gray-950 px-3 py-1.5 text-sm"
        >
          {!modelList || modelList.length === 0 ? (
            <option value="">No models available</option>
          ) : (
            modelList.map((m) => (
              <option key={m.id} value={m.id}>
                {m.id}
              </option>
            ))
          )}
        </select>
      </div>

      <div className="flex-1 overflow-y-auto rounded-lg border border-border bg-surface p-4">
        {messages.length === 0 ? (
          <p className="text-center text-gray-500">
            Send a message to start chatting with the model.
          </p>
        ) : (
          <div className="space-y-4">
            {messages.map((msg, i) => (
              <div
                key={i}
                className={`flex ${msg.role === "user" ? "justify-end" : "justify-start"}`}
              >
                <div
                  className={`max-w-[80%] rounded-lg px-4 py-2 text-sm ${
                    msg.role === "user"
                      ? "bg-accent text-gray-950"
                      : "bg-gray-800 text-gray-200"
                  }`}
                >
                  <p className="whitespace-pre-wrap break-words">
                    {msg.content || (streaming && i === messages.length - 1 ? "▮" : "")}
                  </p>
                </div>
              </div>
            ))}
            {error && (
              <div className="rounded bg-red-900/50 px-3 py-2 text-xs text-red-300">
                {error}
              </div>
            )}
            <div ref={bottomRef} />
          </div>
        )}
      </div>

      <form
        onSubmit={(e) => {
          e.preventDefault();
          send();
        }}
        className="mt-4 flex gap-2"
      >
        <Input
          value={input}
          onChange={(e) => setInput(e.target.value)}
          placeholder="Type a message..."
          disabled={streaming}
          className="flex-1"
        />
        <Button type="submit" disabled={streaming || !input.trim()}>
          Send
        </Button>
      </form>
    </div>
  );
}
