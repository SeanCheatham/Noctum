export const WORKFLOW_PROMPT = `You are setting up a Docker-based simulation environment for this codebase.

Follow these steps in order:

## Step 1: Check Directory
Look for a \`noctum/\` directory in the project root.
- If it doesn't exist, create it using the Bash tool (mkdir noctum)
- Note if this is a first-time setup

## Step 2: Check Docker Compose
Look for \`noctum/docker-compose.yml\`.
- If it exists, skip to Step 4
- If it doesn't exist, continue to Step 3

## Step 3: Generate Docker Compose
Analyze the codebase to understand its structure and dependencies:
- Look for package.json, requirements.txt, Cargo.toml, go.mod, etc.
- Identify the main application entry point
- Determine what services are needed

If you need clarification about the project (database type, external services, ports, etc.), use the AskUserQuestion tool to ask the user.

Then generate an appropriate docker-compose.yml for simulation testing:
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

## Step 4: Build Containers
Run \`docker-compose build\` in the noctum directory.
- Report build progress to the user
- If there are build errors, report them clearly

## Step 5: Run Containers
Run \`docker-compose up\` with a timeout of 2 minutes.
- Success condition: containers run for 2 minutes without crashing
- If there are errors within 2 minutes, report them to the user
- Use \`timeout 120 docker-compose -f noctum/docker-compose.yml up\` or equivalent

Report your progress clearly at each step. Be concise but informative.`;
