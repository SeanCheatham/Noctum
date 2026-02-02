import { query } from "@anthropic-ai/claude-agent-sdk";
import { randomUUID } from "node:crypto";
import type { AgentEvent, AgentState, AgentStatus } from "./types.js";

const PLAN_PROMPT = `You are analyzing a codebase to create a plan for a Docker Compose simulation environment.

Your task:
1. Explore the codebase structure to understand the project
2. Identify the key components and dependencies
3. Design a comprehensive Docker Compose configuration that would:
   - Set up isolated containers for testing
   - Mock external dependencies
   - Provide a sandboxed environment for simulation testing

Output a detailed implementation plan including:
- Service definitions needed
- Network configuration
- Volume mounts for code and data
- Environment variables
- Health checks

Do NOT create any files. Only output a plan.`;

type EventCallback = (event: AgentEvent) => void;

export class AgentService {
  private state: AgentState = {
    status: "idle",
    startedAt: null,
    sessionId: null,
  };

  private listeners = new Map<string, EventCallback>();

  getState(): AgentState {
    return { ...this.state };
  }

  subscribe(id: string, callback: EventCallback): () => void {
    this.listeners.set(id, callback);
    return () => {
      this.listeners.delete(id);
    };
  }

  private emit(event: AgentEvent): void {
    for (const callback of this.listeners.values()) {
      callback(event);
    }
  }

  private setStatus(status: AgentStatus): void {
    this.state.status = status;
    this.emit({ type: "status", timestamp: Date.now(), status });
  }

  async runPlan(workingDirectory: string): Promise<string> {
    if (this.state.status === "running") {
      throw new Error("Agent is already running");
    }

    const sessionId = randomUUID();
    this.state = {
      status: "running",
      startedAt: Date.now(),
      sessionId,
    };

    this.setStatus("running");

    try {
      for await (const message of query({
        prompt: PLAN_PROMPT,
        options: {
          allowedTools: ["Read", "Glob", "Grep"],
          permissionMode: "plan",
          cwd: workingDirectory,
        },
      })) {
        if (message.type === "assistant" && message.message?.content) {
          for (const block of message.message.content) {
            if ("text" in block && typeof block.text === "string") {
              this.emit({ type: "text", timestamp: Date.now(), text: block.text });
            } else if ("name" in block && typeof block.name === "string") {
              this.emit({ type: "tool", timestamp: Date.now(), tool: block.name });
            }
          }
        }
      }

      this.setStatus("completed");
    } catch (err) {
      const errorMessage = err instanceof Error ? err.message : "Unknown error";
      this.emit({ type: "error", timestamp: Date.now(), error: errorMessage });
      this.setStatus("error");
    }

    return sessionId;
  }
}

export const agentService = new AgentService();
