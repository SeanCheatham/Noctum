import { useCallback, useEffect, useRef, useState } from "react";

export type AgentStatus = "idle" | "running" | "completed" | "error";

interface AgentTextEvent {
  type: "text";
  timestamp: number;
  text: string;
}

interface AgentToolEvent {
  type: "tool";
  timestamp: number;
  tool: string;
}

interface AgentStatusEvent {
  type: "status";
  timestamp: number;
  status: AgentStatus;
}

interface AgentErrorEvent {
  type: "error";
  timestamp: number;
  error: string;
}

type AgentEvent = AgentTextEvent | AgentToolEvent | AgentStatusEvent | AgentErrorEvent;

export interface AgentOutput {
  type: "text" | "tool" | "error";
  content: string;
  timestamp: number;
}

interface UseAgentReturn {
  status: AgentStatus;
  output: AgentOutput[];
  run: () => Promise<void>;
  clear: () => void;
}

export function useAgent(): UseAgentReturn {
  const [status, setStatus] = useState<AgentStatus>("idle");
  const [output, setOutput] = useState<AgentOutput[]>([]);
  const eventSourceRef = useRef<EventSource | null>(null);

  useEffect(() => {
    const eventSource = new EventSource("/api/agent/stream");
    eventSourceRef.current = eventSource;

    eventSource.onmessage = (event) => {
      const agentEvent: AgentEvent = JSON.parse(event.data);

      switch (agentEvent.type) {
        case "status":
          setStatus(agentEvent.status);
          break;
        case "text":
          setOutput((prev) => [
            ...prev,
            { type: "text", content: agentEvent.text, timestamp: agentEvent.timestamp },
          ]);
          break;
        case "tool":
          setOutput((prev) => [
            ...prev,
            {
              type: "tool",
              content: `Using tool: ${agentEvent.tool}`,
              timestamp: agentEvent.timestamp,
            },
          ]);
          break;
        case "error":
          setOutput((prev) => [
            ...prev,
            { type: "error", content: agentEvent.error, timestamp: agentEvent.timestamp },
          ]);
          break;
      }
    };

    eventSource.onerror = () => {
      setStatus("error");
    };

    return () => {
      eventSource.close();
    };
  }, []);

  const run = useCallback(async () => {
    setOutput([]);
    try {
      const response = await fetch("/api/agent/run", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({}),
      });
      if (!response.ok) {
        const error = (await response.json()) as { error?: string };
        throw new Error(error.error || "Failed to start agent");
      }
    } catch (err) {
      setStatus("error");
      setOutput([
        {
          type: "error",
          content: err instanceof Error ? err.message : "Unknown error",
          timestamp: Date.now(),
        },
      ]);
    }
  }, []);

  const clear = useCallback(() => {
    setOutput([]);
  }, []);

  return { status, output, run, clear };
}
