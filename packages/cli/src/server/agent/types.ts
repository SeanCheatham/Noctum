export type AgentStatus = "idle" | "running" | "completed" | "error" | "waiting_for_input";

export interface AgentState {
  status: AgentStatus;
  startedAt: number | null;
  sessionId: string | null;
  pendingQuestion: AgentQuestionEvent | null;
  pendingPermission: AgentPermissionEvent | null;
}

export interface QuestionResponse {
  questionId: string;
  answer: string;
}

export interface PermissionResponse {
  permissionId: string;
  approved: boolean;
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

export interface AgentQuestionEvent {
  type: "question";
  timestamp: number;
  questionId: string;
  question: string;
  options?: Array<{ label: string; description: string }>;
}

export interface AgentPermissionEvent {
  type: "permission";
  timestamp: number;
  permissionId: string;
  toolName: "Write" | "Bash";
  description: string;
  command?: string;
}

export type AgentEvent =
  | AgentTextEvent
  | AgentToolEvent
  | AgentStatusEvent
  | AgentErrorEvent
  | AgentQuestionEvent
  | AgentPermissionEvent;
