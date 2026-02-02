export type AgentStatus = "idle" | "running" | "completed" | "error";

export interface AgentState {
  status: AgentStatus;
  startedAt: number | null;
  sessionId: string | null;
}

export interface AgentTextEvent {
  type: "text";
  timestamp: number;
  text: string;
}

export interface AgentToolEvent {
  type: "tool";
  timestamp: number;
  tool: string;
}

export interface AgentStatusEvent {
  type: "status";
  timestamp: number;
  status: AgentStatus;
}

export interface AgentErrorEvent {
  type: "error";
  timestamp: number;
  error: string;
}

export type AgentEvent = AgentTextEvent | AgentToolEvent | AgentStatusEvent | AgentErrorEvent;
