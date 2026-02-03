export type WorkflowType = "compose-generation" | "run-containers";

export const COMPOSE_GENERATION_PROMPT = `You are setting up a Docker-based simulation environment for this codebase.

The noctum/ directory already exists. Your task is to create the necessary Docker configuration files.

## Step 1: Analyze Codebase
Analyze the codebase to understand its structure and dependencies:
- Look for package.json, requirements.txt, Cargo.toml, go.mod, etc.
- Identify the main application entry point
- Determine what services are needed

## Step 2: Gather Information
If you need clarification about the project (database type, external services, ports, etc.), use the AskUserQuestion tool to ask the user.

## Step 3: Generate Dockerfile(s)
Create Dockerfile(s) in noctum/ for services that need custom builds:
- Install all dependencies during the Docker build phase (not at runtime)
- Copy the application code into the image
- Set the CMD to start the application directly

## Step 4: Generate Docker Compose
Generate an appropriate docker-compose.yml for simulation testing:
- Include the main application service
- IMPORTANT: Disable external network access for sandboxing. Use an internal network:
  \`\`\`yaml
  networks:
    sandbox:
      internal: true
  \`\`\`
  And attach all services to this network.
- IMPORTANT: The sandbox has NO external network access. Do NOT include commands that require network access (npm install, pip install, go mod download, etc.) in the container's command or entrypoint. Instead:
  - Use a Dockerfile with multi-stage builds where dependencies are installed during the build phase
  - Or mount the project with pre-installed dependencies via volumes
  - The container command should only start the application, not install dependencies
- Configure appropriate health checks
- Add any necessary volumes
- Reference any Dockerfiles created in Step 3 using the build context

Write the configuration files to the \`noctum/\` directory.

Report your progress clearly. Be concise but informative.`;

export const RUN_CONTAINERS_PROMPT = `You are analyzing the logs from a Docker simulation environment run.

The following logs were captured from running the simulation:
- Cleanup: docker compose down --remove-orphans --volumes
- Build: docker compose build
- Pull: docker compose pull (for pre-built images)
- Run: docker compose up (with 2 minute timeout)

## Logs

{{SIMULATION_LOGS}}

## Your Task

Analyze these logs and provide a clear summary:

1. **Status**: Did the simulation succeed or fail?
2. **Key Events**: What happened during the run?
3. **Issues**: Any errors, exceptions, warnings, or connection problems?
4. **Recommendations**: What should be done next?

Be concise but informative.`;

export function getWorkflowPrompt(workflow: WorkflowType): string {
  switch (workflow) {
    case "compose-generation":
      return COMPOSE_GENERATION_PROMPT;
    case "run-containers":
      return RUN_CONTAINERS_PROMPT;
  }
}

export interface SimulationStep {
  name: string;
  output: string;
  exitCode: number | null;
  timedOut: boolean;
}

export interface SimulationResult {
  success: boolean;
  steps: SimulationStep[];
  combinedLogs: string;
}

type StepCallback = (step: string, output: string) => void;

async function runCommand(
  command: string,
  args: string[],
  cwd: string,
  timeoutMs: number,
  onOutput?: (data: string) => void
): Promise<SimulationStep> {
  const { spawn } = await import("node:child_process");

  return new Promise((resolve) => {
    const name = `${command} ${args.join(" ")}`;
    let output = "";
    let timedOut = false;

    const proc = spawn(command, args, {
      cwd,
      stdio: ["ignore", "pipe", "pipe"],
    });

    const timeout = setTimeout(() => {
      timedOut = true;
      proc.kill("SIGTERM");
      setTimeout(() => proc.kill("SIGKILL"), 5000);
    }, timeoutMs);

    proc.stdout.on("data", (data: Buffer) => {
      const text = data.toString();
      output += text;
      onOutput?.(text);
    });

    proc.stderr.on("data", (data: Buffer) => {
      const text = data.toString();
      output += text;
      onOutput?.(text);
    });

    proc.on("close", (code) => {
      clearTimeout(timeout);
      resolve({ name, output, exitCode: code, timedOut });
    });

    proc.on("error", (err) => {
      clearTimeout(timeout);
      resolve({ name, output: output + `\nError: ${err.message}`, exitCode: 1, timedOut: false });
    });
  });
}

async function stopContainers(cwd: string, composePath: string): Promise<void> {
  const { spawn } = await import("node:child_process");

  return new Promise((resolve) => {
    const proc = spawn("docker", ["compose", "-f", composePath, "down"], {
      cwd,
      stdio: "ignore",
    });

    const timeout = setTimeout(() => {
      proc.kill("SIGKILL");
      resolve();
    }, 10000);

    proc.on("close", () => {
      clearTimeout(timeout);
      resolve();
    });

    proc.on("error", () => {
      clearTimeout(timeout);
      resolve();
    });
  });
}

export async function runDockerSimulation(
  workingDirectory: string,
  onStepStart?: StepCallback,
  onOutput?: (data: string) => void
): Promise<SimulationResult> {
  const composePath = "noctum/docker-compose.yml";
  const steps: SimulationStep[] = [];

  // Step 1: Cleanup
  onStepStart?.("cleanup", "Cleaning up existing containers...");
  const cleanup = await runCommand(
    "docker",
    ["compose", "-f", composePath, "down", "--remove-orphans", "--volumes"],
    workingDirectory,
    30000,
    onOutput
  );
  steps.push(cleanup);

  if (cleanup.exitCode !== 0) {
    return {
      success: false,
      steps,
      combinedLogs: formatCombinedLogs(steps),
    };
  }

  // Step 2: Build
  onStepStart?.("build", "Building containers...");
  const build = await runCommand(
    "docker",
    ["compose", "-f", composePath, "build"],
    workingDirectory,
    300000, // 5 minutes for build
    onOutput
  );
  steps.push(build);

  if (build.exitCode !== 0) {
    return {
      success: false,
      steps,
      combinedLogs: formatCombinedLogs(steps),
    };
  }

  // Step 3: Pull pre-built images
  onStepStart?.("pull", "Pulling images...");
  const pull = await runCommand(
    "docker",
    ["compose", "-f", composePath, "pull", "--ignore-buildable"],
    workingDirectory,
    300000, // 5 minutes for pull
    onOutput
  );
  steps.push(pull);

  if (pull.exitCode !== 0) {
    return {
      success: false,
      steps,
      combinedLogs: formatCombinedLogs(steps),
    };
  }

  // Step 4: Run with 2 minute timeout
  onStepStart?.("run", "Running containers (2 minute timeout)...");
  const run = await runCommand(
    "docker",
    ["compose", "-f", composePath, "up"],
    workingDirectory,
    120000, // 2 minutes
    onOutput
  );
  steps.push(run);

  // Stop containers properly after timeout or completion
  if (run.timedOut) {
    onStepStart?.("stopping", "Stopping containers...");
    await stopContainers(workingDirectory, composePath);
  }

  // Success if it timed out (ran for full 2 minutes) or exited with 0
  const success = run.timedOut || run.exitCode === 0;

  return {
    success,
    steps,
    combinedLogs: formatCombinedLogs(steps),
  };
}

function formatCombinedLogs(steps: SimulationStep[]): string {
  return steps
    .map((step) => {
      const status = step.timedOut
        ? "(timed out - expected)"
        : step.exitCode === 0
          ? "(success)"
          : `(exit code: ${step.exitCode})`;
      return `### ${step.name} ${status}\n\n\`\`\`\n${step.output.trim() || "(no output)"}\n\`\`\``;
    })
    .join("\n\n");
}

export type SandboxValidationResult = { valid: true } | { valid: false; error: string };

export async function validateSandboxConfig(
  workingDirectory: string
): Promise<SandboxValidationResult> {
  const { readFile } = await import("node:fs/promises");
  const { join } = await import("node:path");

  const composePath = join(workingDirectory, "noctum", "docker-compose.yml");

  let content: string;
  try {
    content = await readFile(composePath, "utf-8");
  } catch {
    return { valid: false, error: `docker-compose.yml not found at ${composePath}` };
  }

  // Check for the sandbox network with internal: true
  // This regex matches the networks.sandbox.internal: true pattern in YAML
  const hasSandboxNetwork = /networks:\s*\n\s+sandbox:\s*\n\s+internal:\s*true/m.test(content);

  if (!hasSandboxNetwork) {
    return {
      valid: false,
      error:
        "docker-compose.yml is missing required sandbox network configuration. " +
        "Expected: networks.sandbox.internal: true",
    };
  }

  return { valid: true };
}
