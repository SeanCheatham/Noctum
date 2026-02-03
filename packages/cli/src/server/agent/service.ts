import { query } from "@anthropic-ai/claude-agent-sdk";
import { randomUUID } from "node:crypto";
import type {
  AgentEvent,
  AgentQuestionEvent,
  AgentPermissionEvent,
  AgentState,
  AgentStatus,
  QuestionResponse,
  PermissionResponse,
} from "./types.js";
import {
  getWorkflowPrompt,
  runDockerSimulation,
  RUN_CONTAINERS_PROMPT,
  type WorkflowType,
} from "./workflow.js";

type EventCallback = (event: AgentEvent) => void;

interface PendingResolver {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  timeoutId: NodeJS.Timeout;
}

export class AgentService {
  private state: AgentState = {
    status: "idle",
    startedAt: null,
    sessionId: null,
    pendingQuestion: null,
    pendingPermission: null,
  };

  private eventHistory: AgentEvent[] = [];
  private listeners = new Map<string, EventCallback>();
  private pendingResolvers = new Map<string, PendingResolver>();

  getState(): AgentState {
    return { ...this.state };
  }

  getEventHistory(): AgentEvent[] {
    return [...this.eventHistory];
  }

  reset(): void {
    this.state = {
      status: "idle",
      startedAt: null,
      sessionId: null,
      pendingQuestion: null,
      pendingPermission: null,
    };
    this.eventHistory = [];
  }

  subscribe(id: string, callback: EventCallback): () => void {
    this.listeners.set(id, callback);
    return () => {
      this.listeners.delete(id);
    };
  }

  private emit(event: AgentEvent): void {
    this.eventHistory.push(event);
    for (const callback of this.listeners.values()) {
      callback(event);
    }
  }

  private setStatus(status: AgentStatus): void {
    this.state.status = status;
    this.emit({ type: "status", timestamp: Date.now(), status });
  }

  async respondToQuestion(response: QuestionResponse): Promise<void> {
    const resolver = this.pendingResolvers.get(response.questionId);
    if (!resolver) {
      throw new Error("No pending question with that ID");
    }

    clearTimeout(resolver.timeoutId);
    this.pendingResolvers.delete(response.questionId);
    this.state.pendingQuestion = null;

    this.emit({
      type: "question_answered",
      timestamp: Date.now(),
      questionId: response.questionId,
      answer: response.answer,
    });
    this.setStatus("running");

    resolver.resolve(response.answer);
  }

  async respondToPermission(response: PermissionResponse): Promise<void> {
    const resolver = this.pendingResolvers.get(response.permissionId);
    if (!resolver) {
      throw new Error("No pending permission with that ID");
    }

    clearTimeout(resolver.timeoutId);
    this.pendingResolvers.delete(response.permissionId);
    this.state.pendingPermission = null;

    this.emit({
      type: "permission_responded",
      timestamp: Date.now(),
      permissionId: response.permissionId,
      approved: response.approved,
    });
    this.setStatus("running");

    resolver.resolve(response.approved);
  }

  private waitForResponse<T>(id: string, timeoutMs: number): Promise<T> {
    return new Promise((resolve, reject) => {
      const timeoutId = setTimeout(() => {
        this.pendingResolvers.delete(id);
        this.setStatus("error");
        reject(new Error("Response timeout - user did not respond in time"));
      }, timeoutMs);

      this.pendingResolvers.set(id, {
        resolve: resolve as (value: unknown) => void,
        reject,
        timeoutId,
      });
    });
  }

  private async handleAskUserQuestion(
    input: Record<string, unknown>
  ): Promise<{ behavior: "allow"; updatedInput: Record<string, unknown> }> {
    const questions = input.questions as Array<{
      question: string;
      header?: string;
      options?: Array<{ label: string; description: string }>;
      multiSelect?: boolean;
    }>;

    if (!questions || questions.length === 0) {
      return { behavior: "allow", updatedInput: input };
    }

    const question = questions[0];
    const questionId = randomUUID();

    const questionEvent: AgentQuestionEvent = {
      type: "question",
      timestamp: Date.now(),
      questionId,
      question: question.question,
      options: question.options,
    };

    this.state.pendingQuestion = questionEvent;
    this.setStatus("waiting_for_input");
    this.emit(questionEvent);

    const answer = await this.waitForResponse<string>(questionId, 120000);

    const answers: Record<string, string> = {};
    answers[question.question] = answer;

    return {
      behavior: "allow",
      updatedInput: {
        questions: input.questions,
        answers,
      },
    };
  }

  private async handleToolPermission(
    toolName: string,
    input: Record<string, unknown>
  ): Promise<{ behavior: "allow" } | { behavior: "deny"; message: string }> {
    const permissionId = randomUUID();

    let description = `Execute ${toolName}`;
    let command: string | undefined;

    if (toolName === "Bash" && typeof input.command === "string") {
      description = `Run shell command`;
      command = input.command;
    } else if (toolName === "Write" && typeof input.file_path === "string") {
      description = `Write to file: ${input.file_path}`;
    }

    const permissionEvent: AgentPermissionEvent = {
      type: "permission",
      timestamp: Date.now(),
      permissionId,
      toolName: toolName as "Write" | "Bash",
      description,
      command,
    };

    this.state.pendingPermission = permissionEvent;
    this.setStatus("waiting_for_input");
    this.emit(permissionEvent);

    const approved = await this.waitForResponse<boolean>(permissionId, 120000);

    if (!approved) {
      return { behavior: "deny", message: "User denied permission" };
    }

    return { behavior: "allow" };
  }

  async runWorkflow(workingDirectory: string, workflow: WorkflowType): Promise<string> {
    if (this.state.status === "running" || this.state.status === "waiting_for_input") {
      throw new Error("Agent is already running");
    }

    const sessionId = randomUUID();
    this.state = {
      status: "running",
      startedAt: Date.now(),
      sessionId,
      pendingQuestion: null,
      pendingPermission: null,
    };
    this.eventHistory = [];

    this.setStatus("running");

    try {
      if (workflow === "run-containers") {
        await this.runContainersWorkflow(workingDirectory);
      } else {
        await this.runAgentWorkflow(workingDirectory, workflow);
      }
      this.setStatus("completed");
    } catch (err) {
      const errorMessage = err instanceof Error ? err.message : "Unknown error";
      this.emit({ type: "error", timestamp: Date.now(), error: errorMessage });
      this.setStatus("error");
    }

    return sessionId;
  }

  private async runContainersWorkflow(workingDirectory: string): Promise<void> {
    // Run docker commands automatically
    const result = await runDockerSimulation(
      workingDirectory,
      (step, message) => {
        this.emit({ type: "text", timestamp: Date.now(), text: `**${step}**: ${message}` });
      },
      (data) => {
        this.emit({ type: "text", timestamp: Date.now(), text: data });
      }
    );

    // Prepare the prompt with logs
    const prompt = RUN_CONTAINERS_PROMPT.replace("{{SIMULATION_LOGS}}", result.combinedLogs);

    // Run LLM to analyze logs
    this.emit({ type: "text", timestamp: Date.now(), text: "\n---\n**Analyzing results...**\n" });

    for await (const message of query({
      prompt,
      options: {
        allowedTools: [],
        cwd: workingDirectory,
      },
    })) {
      if (message.type === "assistant" && message.message?.content) {
        for (const block of message.message.content) {
          if ("text" in block && typeof block.text === "string") {
            this.emit({ type: "text", timestamp: Date.now(), text: block.text });
          }
        }
      }
    }
  }

  private async runAgentWorkflow(workingDirectory: string, workflow: WorkflowType): Promise<void> {
    const prompt = getWorkflowPrompt(workflow);

    for await (const message of query({
      prompt,
      options: {
        allowedTools: ["Read", "Glob", "Grep", "Write", "Bash", "AskUserQuestion"],
        cwd: workingDirectory,
        canUseTool: async (
          toolName: string,
          input: Record<string, unknown>
        ): Promise<
          | { behavior: "allow"; updatedInput?: Record<string, unknown> }
          | { behavior: "deny"; message: string }
        > => {
          if (toolName === "AskUserQuestion") {
            return this.handleAskUserQuestion(input);
          }

          if (toolName === "Write" || toolName === "Bash") {
            return this.handleToolPermission(toolName, input);
          }

          return { behavior: "allow" };
        },
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
  }
}

export const agentService = new AgentService();
