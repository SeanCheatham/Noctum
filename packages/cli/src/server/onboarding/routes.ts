import type { IncomingMessage, ServerResponse } from "node:http";
import { sendJson } from "../routes.js";
import { OnboardingService } from "./service.js";
import { onboardingState } from "./state.js";
import type { OnboardingStep } from "./types.js";

export function handleGetState(_req: IncomingMessage, res: ServerResponse): void {
  const state = onboardingState.getState();
  sendJson(res, 200, { success: true, data: state });
}

export async function handleSetStep(req: IncomingMessage, res: ServerResponse): Promise<void> {
  let body = "";
  for await (const chunk of req) {
    body += chunk;
  }

  if (!body) {
    sendJson(res, 400, { success: false, error: "Missing request body" });
    return;
  }

  let parsed: { step: OnboardingStep };
  try {
    parsed = JSON.parse(body) as { step: OnboardingStep };
  } catch {
    sendJson(res, 400, { success: false, error: "Invalid JSON body" });
    return;
  }

  if (!parsed.step) {
    sendJson(res, 400, { success: false, error: "Missing step" });
    return;
  }

  onboardingState.setStep(parsed.step);
  sendJson(res, 200, { success: true });
}

export function handleResetState(_req: IncomingMessage, res: ServerResponse): void {
  onboardingState.reset();
  sendJson(res, 200, { success: true });
}

export function handleGitStatus(_req: IncomingMessage, res: ServerResponse): void {
  const service = new OnboardingService(process.cwd());
  const result = service.checkGitStatus();
  onboardingState.updateState({
    gitStatus: result,
    step: result.isClean ? "directory-check" : "git-status",
  });
  sendJson(res, 200, { success: true, data: result });
}

export function handleCheckDirectory(_req: IncomingMessage, res: ServerResponse): void {
  const service = new OnboardingService(process.cwd());
  const result = service.checkNoctumDirectory();
  onboardingState.updateState({
    directoryStatus: result,
    step: result.exists ? "compose-check" : "directory-check",
  });
  sendJson(res, 200, { success: true, data: result });
}

export function handleCreateDirectory(_req: IncomingMessage, res: ServerResponse): void {
  const service = new OnboardingService(process.cwd());
  try {
    service.createNoctumDirectory();
    onboardingState.setStep("compose-check");
    sendJson(res, 200, { success: true });
  } catch (err) {
    const message = err instanceof Error ? err.message : "Failed to create directory";
    sendJson(res, 500, { success: false, error: message });
  }
}

export function handleCheckCompose(_req: IncomingMessage, res: ServerResponse): void {
  const cwd = process.cwd();
  const service = new OnboardingService(cwd);
  const result = service.checkDockerCompose();
  onboardingState.updateState({
    composeStatus: result,
    step: result.exists ? "ready" : "compose-generation",
  });
  sendJson(res, 200, { success: true, data: result });
}
