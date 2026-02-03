import { describe, it, expect, afterEach } from "vitest";
import { startServer } from "../../server/index.js";
import type { ApiResponse, HealthData } from "../../server/types.js";

describe("server", () => {
  let shutdown: (() => Promise<void>) | null = null;

  afterEach(async () => {
    if (shutdown) {
      await shutdown();
      shutdown = null;
    }
  });

  it("should start on a random port when port is 0", async () => {
    const result = await startServer({ port: 0 });
    shutdown = result.shutdown;

    expect(result.info.port).toBeGreaterThan(0);
    expect(result.info.host).toBe("127.0.0.1");
    expect(result.info.url).toContain("http://127.0.0.1:");
  });

  it("should return health status at /api/health", async () => {
    const result = await startServer({ port: 0 });
    shutdown = result.shutdown;

    const response = await fetch(`${result.info.url}api/health`);
    expect(response.status).toBe(200);

    const data = (await response.json()) as ApiResponse<HealthData>;
    expect(data.success).toBe(true);
    expect(data.data?.status).toBe("ok");
    expect(data.data?.version).toBeDefined();
  });

  it("should return 404 for unknown API routes", async () => {
    const result = await startServer({ port: 0 });
    shutdown = result.shutdown;

    const response = await fetch(`${result.info.url}api/unknown`);
    expect(response.status).toBe(404);

    const data = (await response.json()) as ApiResponse;
    expect(data.success).toBe(false);
    expect(data.error).toBe("Not found");
  });
});
