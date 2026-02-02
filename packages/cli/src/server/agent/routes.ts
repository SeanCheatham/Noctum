import type { IncomingMessage, ServerResponse } from "node:http";
import { randomUUID } from "node:crypto";
import { agentService } from "./service.js";
import { sendJson } from "../routes.js";

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
  if (body) {
    try {
      const parsed = JSON.parse(body) as { workingDirectory?: string };
      if (parsed.workingDirectory) {
        workingDirectory = parsed.workingDirectory;
      }
    } catch {
      sendJson(res, 400, { success: false, error: "Invalid JSON body" });
      return;
    }
  }

  try {
    const sessionId = await agentService.runPlan(workingDirectory);
    sendJson(res, 200, { success: true, data: { sessionId } });
  } catch (err) {
    const message = err instanceof Error ? err.message : "Failed to start agent";
    sendJson(res, 500, { success: false, error: message });
  }
}

export function handleAgentStream(_req: IncomingMessage, res: ServerResponse): void {
  res.writeHead(200, {
    "Content-Type": "text/event-stream",
    "Cache-Control": "no-cache",
    Connection: "keep-alive",
  });

  const subscriberId = randomUUID();

  const state = agentService.getState();
  res.write(
    `data: ${JSON.stringify({ type: "status", timestamp: Date.now(), status: state.status })}\n\n`
  );

  const unsubscribe = agentService.subscribe(subscriberId, (event) => {
    res.write(`data: ${JSON.stringify(event)}\n\n`);
  });

  _req.on("close", () => {
    unsubscribe();
  });
}
