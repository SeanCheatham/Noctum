import type { IncomingMessage, ServerResponse } from "node:http";

export type { ApiResponse, HealthData } from "@noctum/shared";

// Server-specific types
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
