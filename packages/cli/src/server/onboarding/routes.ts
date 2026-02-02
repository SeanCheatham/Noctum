import type { IncomingMessage, ServerResponse } from "node:http";
import { sendJson } from "../routes.js";
import { OnboardingService } from "./service.js";

export function handleGitStatus(_req: IncomingMessage, res: ServerResponse): void {
  const service = new OnboardingService(process.cwd());
  const result = service.checkGitStatus();
  sendJson(res, 200, { success: true, data: result });
}

export function handleCheckDirectory(_req: IncomingMessage, res: ServerResponse): void {
  const service = new OnboardingService(process.cwd());
  const result = service.checkNoctumDirectory();
  sendJson(res, 200, { success: true, data: result });
}

export function handleCreateDirectory(_req: IncomingMessage, res: ServerResponse): void {
  const service = new OnboardingService(process.cwd());
  try {
    service.createNoctumDirectory();
    sendJson(res, 200, { success: true });
  } catch (err) {
    const message = err instanceof Error ? err.message : "Failed to create directory";
    sendJson(res, 500, { success: false, error: message });
  }
}

export function handleCheckCompose(_req: IncomingMessage, res: ServerResponse): void {
  const service = new OnboardingService(process.cwd());
  const result = service.checkDockerCompose();
  sendJson(res, 200, { success: true, data: result });
}
