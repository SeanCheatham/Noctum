import type { IncomingMessage, ServerResponse } from "node:http";

export interface ServerConfig {
  host?: string;
  port?: number;
}

export interface ServerInfo {
  url: string;
  port: number;
  host: string;
}

export type RouteHandler = (req: IncomingMessage, res: ServerResponse) => Promise<void> | void;

export interface ApiResponse<T = unknown> {
  success: boolean;
  data?: T;
  error?: string;
}

export interface HealthResponse {
  status: "ok";
  version: string;
  uptime: number;
}
