import http, { type IncomingMessage, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";
import {
  handleAgentRun,
  handleAgentStatus,
  handleAgentStream,
  handleAgentReset,
  handleRespondQuestion,
  handleRespondPermission,
} from "./agent/index.js";
import {
  handleGetState,
  handleSetStep,
  handleResetState,
  handleGitStatus,
  handleCheckDirectory,
  handleCreateDirectory,
  handleCheckCompose,
} from "./onboarding/index.js";
import { handleHealth, handleNotFound } from "./routes.js";
import { serveStaticFile } from "./static.js";
import type { ServerConfig, ServerInfo } from "./types.js";

export type { ServerConfig, ServerInfo };

function createHandler() {
  return async (req: IncomingMessage, res: ServerResponse): Promise<void> => {
    const url = new URL(req.url ?? "/", `http://${req.headers.host}`);
    const path = url.pathname;
    const method = req.method ?? "GET";

    // API routes
    if (path === "/api/health" && method === "GET") {
      handleHealth(req, res);
      return;
    }

    if (path === "/api/agent/status" && method === "GET") {
      handleAgentStatus(req, res);
      return;
    }

    if (path === "/api/agent/run" && method === "POST") {
      await handleAgentRun(req, res);
      return;
    }

    if (path === "/api/agent/stream" && method === "GET") {
      handleAgentStream(req, res);
      return;
    }

    if (path === "/api/agent/respond-question" && method === "POST") {
      await handleRespondQuestion(req, res);
      return;
    }

    if (path === "/api/agent/respond-permission" && method === "POST") {
      await handleRespondPermission(req, res);
      return;
    }

    if (path === "/api/agent/reset" && method === "POST") {
      handleAgentReset(req, res);
      return;
    }

    // Onboarding routes
    if (path === "/api/onboarding/state" && method === "GET") {
      handleGetState(req, res);
      return;
    }

    if (path === "/api/onboarding/step" && method === "POST") {
      await handleSetStep(req, res);
      return;
    }

    if (path === "/api/onboarding/reset" && method === "POST") {
      handleResetState(req, res);
      return;
    }

    if (path === "/api/onboarding/git-status" && method === "GET") {
      handleGitStatus(req, res);
      return;
    }

    if (path === "/api/onboarding/check-directory" && method === "GET") {
      handleCheckDirectory(req, res);
      return;
    }

    if (path === "/api/onboarding/create-directory" && method === "POST") {
      handleCreateDirectory(req, res);
      return;
    }

    if (path === "/api/onboarding/check-compose" && method === "GET") {
      handleCheckCompose(req, res);
      return;
    }

    // Unknown API routes return 404
    if (path.startsWith("/api/")) {
      handleNotFound(req, res);
      return;
    }

    // Serve static files (frontend)
    if (method === "GET") {
      try {
        const file = await serveStaticFile(path);
        if (file) {
          res.writeHead(200, { "Content-Type": file.contentType });
          res.end(file.content);
          return;
        }
      } catch {
        // Fall through to 404
      }
    }

    handleNotFound(req, res);
  };
}

export function startServer(config: ServerConfig = {}): Promise<{
  server: http.Server;
  info: ServerInfo;
  shutdown: () => Promise<void>;
}> {
  const host = config.host ?? "127.0.0.1";
  const port = config.port ?? 0;

  return new Promise((resolve, reject) => {
    const server = http.createServer(createHandler());

    server.on("error", reject);

    server.listen(port, host, () => {
      const addr = server.address() as AddressInfo;
      const info: ServerInfo = {
        url: `http://${addr.address}:${addr.port}/`,
        port: addr.port,
        host: addr.address,
      };

      const shutdown = (): Promise<void> => {
        return new Promise((res, rej) => {
          server.closeAllConnections();
          server.close((err) => (err ? rej(err) : res()));
        });
      };

      resolve({ server, info, shutdown });
    });
  });
}
