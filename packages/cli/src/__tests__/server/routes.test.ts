import { describe, it, expect, vi } from "vitest";
import type { IncomingMessage, ServerResponse } from "node:http";
import { handleHealth, handleNotFound, sendJson } from "../../server/routes.js";

describe("routes", () => {
  describe("sendJson", () => {
    it("should send JSON response with correct headers", () => {
      const res = {
        writeHead: vi.fn(),
        end: vi.fn(),
      } as unknown as ServerResponse;

      sendJson(res, 200, { success: true, data: { test: true } });

      expect(res.writeHead).toHaveBeenCalledWith(200, { "Content-Type": "application/json" });
      expect(res.end).toHaveBeenCalledWith('{"success":true,"data":{"test":true}}');
    });

    it("should handle error responses", () => {
      const res = {
        writeHead: vi.fn(),
        end: vi.fn(),
      } as unknown as ServerResponse;

      sendJson(res, 500, { success: false, error: "Internal error" });

      expect(res.writeHead).toHaveBeenCalledWith(500, { "Content-Type": "application/json" });
      expect(res.end).toHaveBeenCalledWith('{"success":false,"error":"Internal error"}');
    });
  });

  describe("handleHealth", () => {
    it("should return health status with version", () => {
      const req = {} as IncomingMessage;
      const res = {
        writeHead: vi.fn(),
        end: vi.fn(),
      } as unknown as ServerResponse;

      handleHealth(req, res);

      expect(res.writeHead).toHaveBeenCalledWith(200, { "Content-Type": "application/json" });

      const endFn = res.end as ReturnType<typeof vi.fn>;
      const response = JSON.parse(endFn.mock.calls[0][0]);

      expect(response.success).toBe(true);
      expect(response.data.status).toBe("ok");
      expect(response.data.version).toBeDefined();
      expect(typeof response.data.uptime).toBe("number");
    });
  });

  describe("handleNotFound", () => {
    it("should return 404 error", () => {
      const req = {} as IncomingMessage;
      const res = {
        writeHead: vi.fn(),
        end: vi.fn(),
      } as unknown as ServerResponse;

      handleNotFound(req, res);

      expect(res.writeHead).toHaveBeenCalledWith(404, { "Content-Type": "application/json" });

      const endFn = res.end as ReturnType<typeof vi.fn>;
      const response = JSON.parse(endFn.mock.calls[0][0]);

      expect(response.success).toBe(false);
      expect(response.error).toBe("Not found");
    });
  });
});
