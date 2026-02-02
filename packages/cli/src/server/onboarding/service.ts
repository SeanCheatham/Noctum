import { execSync } from "node:child_process";
import { existsSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import type { GitStatusResult, DirectoryCheckResult, ComposeCheckResult } from "./types.js";

export class OnboardingService {
  constructor(private workingDirectory: string) {}

  checkGitStatus(): GitStatusResult {
    try {
      const branchOutput = execSync("git rev-parse --abbrev-ref HEAD", {
        cwd: this.workingDirectory,
        encoding: "utf-8",
      }).trim();

      const statusOutput = execSync("git status --porcelain", {
        cwd: this.workingDirectory,
        encoding: "utf-8",
      }).trim();

      const changedFiles = statusOutput
        ? statusOutput.split("\n").map((line) => line.slice(3))
        : [];

      return {
        isClean: changedFiles.length === 0,
        branch: branchOutput,
        changedFiles,
      };
    } catch {
      // Not a git repo or git not available
      return {
        isClean: true,
        branch: "",
        changedFiles: [],
      };
    }
  }

  checkNoctumDirectory(): DirectoryCheckResult {
    const noctumPath = join(this.workingDirectory, "noctum");
    return {
      exists: existsSync(noctumPath),
    };
  }

  checkDockerCompose(): ComposeCheckResult {
    const composePath = join(this.workingDirectory, "noctum", "docker-compose.yml");
    return {
      exists: existsSync(composePath),
      path: composePath,
    };
  }

  createNoctumDirectory(): void {
    const noctumPath = join(this.workingDirectory, "noctum");
    if (!existsSync(noctumPath)) {
      mkdirSync(noctumPath, { recursive: true });
    }
  }
}
