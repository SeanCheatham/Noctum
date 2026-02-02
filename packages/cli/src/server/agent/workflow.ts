export type WorkflowType = "compose-generation" | "run-containers";

export const COMPOSE_GENERATION_PROMPT = `You are setting up a Docker-based simulation environment for this codebase.

The noctum/ directory already exists. Your task is to create the docker-compose.yml file.

## Step 1: Analyze Codebase
Analyze the codebase to understand its structure and dependencies:
- Look for package.json, requirements.txt, Cargo.toml, go.mod, etc.
- Identify the main application entry point
- Determine what services are needed

## Step 2: Gather Information
If you need clarification about the project (database type, external services, ports, etc.), use the AskUserQuestion tool to ask the user.

## Step 3: Generate Docker Compose
Generate an appropriate docker-compose.yml for simulation testing:
- Include the main application service
- IMPORTANT: Disable external network access for sandboxing. Use an internal network:
  \`\`\`yaml
  networks:
    sandbox:
      internal: true
  \`\`\`
  And attach all services to this network.
- Configure appropriate health checks
- Add any necessary volumes

Write the configuration to \`noctum/docker-compose.yml\`.

Report your progress clearly. Be concise but informative.`;

export const RUN_CONTAINERS_PROMPT = `You are running the Docker simulation environment.

The docker-compose.yml already exists at noctum/docker-compose.yml.

## Step 1: Build Containers
Run \`docker-compose -f noctum/docker-compose.yml build\` to build the containers.
- Report build progress to the user
- If there are build errors, report them clearly

## Step 2: Run Containers
Run the containers with a timeout of 2 minutes.
- Success condition: containers run for 2 minutes without crashing
- If there are errors within 2 minutes, report them to the user
- Use \`timeout 120 docker-compose -f noctum/docker-compose.yml up\` or equivalent

Report your progress clearly. Be concise but informative.`;

export function getWorkflowPrompt(workflow: WorkflowType): string {
  switch (workflow) {
    case "compose-generation":
      return COMPOSE_GENERATION_PROMPT;
    case "run-containers":
      return RUN_CONTAINERS_PROMPT;
  }
}
