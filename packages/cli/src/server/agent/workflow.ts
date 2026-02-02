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

export const RUN_CONTAINERS_PROMPT = `You are running the Docker simulation environment.

The docker-compose.yml already exists at noctum/docker-compose.yml.

## Step 1: Cleanup
Run \`docker-compose -f noctum/docker-compose.yml down --remove-orphans --volumes\` to clean up any existing containers, networks, and volumes from previous runs.

## Step 2: Build Containers
Run \`docker-compose -f noctum/docker-compose.yml build\` to build the containers.
- Report build progress to the user
- If there are build errors, report them clearly

## Step 3: Run Containers
Run the containers with a timeout of 2 minutes.
- Use \`timeout 120 docker-compose -f noctum/docker-compose.yml up 2>&1\` or equivalent
- Capture all output (stdout and stderr)
- Success condition: containers run for 2 minutes without crashing

## Step 4: Analyze Results
After the containers stop (timeout or crash), analyze the logs:
- Identify any errors, exceptions, or warnings
- Check if services started successfully
- Look for connection issues or dependency problems
- Summarize what happened during the run

Provide a clear summary of:
1. Whether the simulation succeeded or failed
2. Key events that occurred
3. Any issues that need attention
4. Recommendations for next steps

Report your progress clearly. Be concise but informative.`;

export function getWorkflowPrompt(workflow: WorkflowType): string {
  switch (workflow) {
    case "compose-generation":
      return COMPOSE_GENERATION_PROMPT;
    case "run-containers":
      return RUN_CONTAINERS_PROMPT;
  }
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
