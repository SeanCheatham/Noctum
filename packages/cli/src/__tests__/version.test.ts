import { describe, it, expect } from "vitest";
import { version } from "../version.js";

describe("version", () => {
  it("should be a valid semver string", () => {
    expect(version).toMatch(/^\d+\.\d+\.\d+$/);
  });

  it("should match package.json version", async () => {
    const pkg = await import("../../package.json", {
      with: { type: "json" },
    });
    expect(version).toBe(pkg.default.version);
  });
});
