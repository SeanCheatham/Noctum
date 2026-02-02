import type { IncomingMessage, ServerResponse } from "node:http";
import { randomUUID } from "node:crypto";
import { agentService } from "./service.js";
import { sendJson } from "../routes.js";
import type { QuestionResponse, PermissionResponse } from "./types.js";
import { validateSandboxConfig, type WorkflowType } from "./workflow.js";

export function handleAgentStatus(_req: IncomingMessage, res: ServerResponse): void {
  const state = agentService.getState();
  sendJson(res, 200, { success: true, data: state });
}

export async function handleAgentRun(req: IncomingMessage, res: ServerResponse): Promise<void> {
  let body = "";
  for await (const chunk of req) {
    body += chunk;
  }

  let workingDirectory = process.cwd();
  let workflow: WorkflowType = "run-containers";

  if (body) {
    try {
      const parsed = JSON.parse(body) as {
        workingDirectory?: string;
        workflow?: WorkflowType;
      };
      if (parsed.workingDirectory) {
        workingDirectory = parsed.workingDirectory;
      }
      if (parsed.workflow) {
        workflow = parsed.workflow;
      }
    } catch {
      sendJson(res, 400, { success: false, error: "Invalid JSON body" });
      return;
    }
  }

  // Validate sandbox configuration before running containers
  if (workflow === "run-containers") {
    const validation = await validateSandboxConfig(workingDirectory);
    if (!validation.valid) {
      sendJson(res, 400, { success: false, error: validation.error });
      return;
    }
  }

  try {
    const sessionId = await agentService.runWorkflow(workingDirectory, workflow);
    sendJson(res, 200, { success: true, data: { sessionId } });
  } catch (err) {
    const message = err instanceof Error ? err.message : "Failed to start agent";
    sendJson(res, 500, { success: false, error: message });
  }
}

export async function handleRespondQuestion(
  req: IncomingMessage,
  res: ServerResponse
): Promise<void> {
  let body = "";
  for await (const chunk of req) {
    body += chunk;
  }

  if (!body) {
    sendJson(res, 400, { success: false, error: "Missing request body" });
    return;
  }

  let parsed: QuestionResponse;
  try {
    parsed = JSON.parse(body) as QuestionResponse;
  } catch {
    sendJson(res, 400, { success: false, error: "Invalid JSON body" });
    return;
  }

  if (!parsed.questionId || typeof parsed.answer !== "string") {
    sendJson(res, 400, { success: false, error: "Missing questionId or answer" });
    return;
  }

  try {
    await agentService.respondToQuestion(parsed);
    sendJson(res, 200, { success: true });
  } catch (err) {
    const message = err instanceof Error ? err.message : "Failed to respond to question";
    sendJson(res, 400, { success: false, error: message });
  }
}

export async function handleRespondPermission(
  req: IncomingMessage,
  res: ServerResponse
): Promise<void> {
  let body = "";
  for await (const chunk of req) {
    body += chunk;
  }

  if (!body) {
    sendJson(res, 400, { success: false, error: "Missing request body" });
    return;
  }

  let parsed: PermissionResponse;
  try {
    parsed = JSON.parse(body) as PermissionResponse;
  } catch {
    sendJson(res, 400, { success: false, error: "Invalid JSON body" });
    return;
  }

  if (!parsed.permissionId || typeof parsed.approved !== "boolean") {
    sendJson(res, 400, { success: false, error: "Missing permissionId or approved" });
    return;
  }

  try {
    await agentService.respondToPermission(parsed);
    sendJson(res, 200, { success: true });
  } catch (err) {
    const message = err instanceof Error ? err.message : "Failed to respond to permission";
    sendJson(res, 400, { success: false, error: message });
  }
}

export function handleAgentStream(_req: IncomingMessage, res: ServerResponse): void {
  res.writeHead(200, {
    "Content-Type": "text/event-stream",
    "Cache-Control": "no-cache",
    Connection: "keep-alive",
  });

  const subscriberId = randomUUID();

  // Send full event history on connect for session recovery
  const history = agentService.getEventHistory();
  for (const event of history) {
    res.write(`data: ${JSON.stringify(event)}\n\n`);
  }

  // Send current status if no history (idle state)
  if (history.length === 0) {
    const state = agentService.getState();
    res.write(
      `data: ${JSON.stringify({ type: "status", timestamp: Date.now(), status: state.status })}\n\n`
    );
  }

  const unsubscribe = agentService.subscribe(subscriberId, (event) => {
    res.write(`data: ${JSON.stringify(event)}\n\n`);
  });

  _req.on("close", () => {
    unsubscribe();
  });
}

export function handleAgentReset(_req: IncomingMessage, res: ServerResponse): void {
  agentService.reset();
  sendJson(res, 200, { success: true });
}
