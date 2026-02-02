import { useCallback, useEffect, useRef, useState } from "react";

export type AgentStatus = "idle" | "running" | "completed" | "error" | "waiting_for_input";

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

interface AgentQuestionEvent {
  type: "question";
  timestamp: number;
  questionId: string;
  question: string;
  options?: Array<{ label: string; description: string }>;
}

interface AgentPermissionEvent {
  type: "permission";
  timestamp: number;
  permissionId: string;
  toolName: "Write" | "Bash";
  description: string;
  command?: string;
}

type AgentEvent =
  | AgentTextEvent
  | AgentToolEvent
  | AgentStatusEvent
  | AgentErrorEvent
  | AgentQuestionEvent
  | AgentPermissionEvent;

export interface AgentOutput {
  type: "text" | "tool" | "error" | "question" | "permission";
  content: string;
  timestamp: number;
  questionId?: string;
  permissionId?: string;
  options?: Array<{ label: string; description: string }>;
  command?: string;
  status?: "pending" | "answered" | "approved" | "denied";
}

export interface PendingQuestion {
  questionId: string;
  question: string;
  options?: Array<{ label: string; description: string }>;
}

export interface PendingPermission {
  permissionId: string;
  toolName: "Write" | "Bash";
  description: string;
  command?: string;
}

export type WorkflowType = "compose-generation" | "run-containers";

interface RunOptions {
  workflow?: WorkflowType;
}

interface UseAgentReturn {
  status: AgentStatus;
  output: AgentOutput[];
  pendingQuestion: PendingQuestion | null;
  pendingPermission: PendingPermission | null;
  run: (options?: RunOptions) => Promise<void>;
  clear: () => void;
  respondToQuestion: (questionId: string, answer: string) => Promise<void>;
  respondToPermission: (permissionId: string, approved: boolean) => Promise<void>;
}

export function useAgent(): UseAgentReturn {
  const [status, setStatus] = useState<AgentStatus>("idle");
  const [output, setOutput] = useState<AgentOutput[]>([]);
  const [pendingQuestion, setPendingQuestion] = useState<PendingQuestion | null>(null);
  const [pendingPermission, setPendingPermission] = useState<PendingPermission | null>(null);
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
        case "question":
          setPendingQuestion({
            questionId: agentEvent.questionId,
            question: agentEvent.question,
            options: agentEvent.options,
          });
          setOutput((prev) => [
            ...prev,
            {
              type: "question",
              content: agentEvent.question,
              timestamp: agentEvent.timestamp,
              questionId: agentEvent.questionId,
              options: agentEvent.options,
              status: "pending",
            },
          ]);
          break;
        case "permission":
          setPendingPermission({
            permissionId: agentEvent.permissionId,
            toolName: agentEvent.toolName,
            description: agentEvent.description,
            command: agentEvent.command,
          });
          setOutput((prev) => [
            ...prev,
            {
              type: "permission",
              content: agentEvent.description,
              timestamp: agentEvent.timestamp,
              permissionId: agentEvent.permissionId,
              command: agentEvent.command,
              status: "pending",
            },
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

  const run = useCallback(async (options?: RunOptions) => {
    setOutput([]);
    setPendingQuestion(null);
    setPendingPermission(null);
    try {
      const response = await fetch("/api/agent/run", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ workflow: options?.workflow }),
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
    setPendingQuestion(null);
    setPendingPermission(null);
  }, []);

  const respondToQuestion = useCallback(async (questionId: string, answer: string) => {
    try {
      const response = await fetch("/api/agent/respond-question", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ questionId, answer }),
      });
      if (!response.ok) {
        const error = (await response.json()) as { error?: string };
        throw new Error(error.error || "Failed to respond to question");
      }
      setPendingQuestion(null);
      setOutput((prev) =>
        prev.map((item) =>
          item.questionId === questionId ? { ...item, status: "answered" as const } : item
        )
      );
    } catch (err) {
      setOutput((prev) => [
        ...prev,
        {
          type: "error",
          content: err instanceof Error ? err.message : "Failed to respond",
          timestamp: Date.now(),
        },
      ]);
    }
  }, []);

  const respondToPermission = useCallback(async (permissionId: string, approved: boolean) => {
    try {
      const response = await fetch("/api/agent/respond-permission", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ permissionId, approved }),
      });
      if (!response.ok) {
        const error = (await response.json()) as { error?: string };
        throw new Error(error.error || "Failed to respond to permission");
      }
      setPendingPermission(null);
      setOutput((prev) =>
        prev.map((item) =>
          item.permissionId === permissionId
            ? { ...item, status: approved ? ("approved" as const) : ("denied" as const) }
            : item
        )
      );
    } catch (err) {
      setOutput((prev) => [
        ...prev,
        {
          type: "error",
          content: err instanceof Error ? err.message : "Failed to respond",
          timestamp: Date.now(),
        },
      ]);
    }
  }, []);

  return {
    status,
    output,
    pendingQuestion,
    pendingPermission,
    run,
    clear,
    respondToQuestion,
    respondToPermission,
  };
}
