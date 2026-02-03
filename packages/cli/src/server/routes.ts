import type { IncomingMessage, ServerResponse } from "node:http";
import { version } from "../version.js";
import type { ApiResponse, HealthData } from "./types.js";

const startTime = Date.now();

export function sendJson<T>(res: ServerResponse, status: number, data: ApiResponse<T>): void {
  res.writeHead(status, { "Content-Type": "application/json" });
  res.end(JSON.stringify(data));
}

export function handleHealth(_req: IncomingMessage, res: ServerResponse): void {
  const response: ApiResponse<HealthData> = {
    success: true,
    data: {
      status: "ok",
      version,
      uptime: Math.floor((Date.now() - startTime) / 1000),
    },
  };
  sendJson(res, 200, response);
}

export function handleNotFound(_req: IncomingMessage, res: ServerResponse): void {
  sendJson(res, 404, { success: false, error: "Not found" });
}
