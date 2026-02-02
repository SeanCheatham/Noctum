import { AgentOutput } from "../AgentOutput";
import type { AgentOutput as AgentOutputType, AgentStatus } from "../../hooks/useAgent";

interface ComposeGenerationStepProps {
  agentOutput: AgentOutputType[];
  agentStatus: AgentStatus;
  onStart: () => void;
  onComplete: () => void;
  onRespondQuestion: (questionId: string, answer: string) => Promise<void>;
  onRespondPermission: (permissionId: string, approved: boolean) => Promise<void>;
}

export function ComposeGenerationStep({
  agentOutput,
  agentStatus,
  onStart,
  onComplete,
  onRespondQuestion,
  onRespondPermission,
}: ComposeGenerationStepProps) {
  const hasStarted = agentStatus !== "idle";
  const isCompleted = agentStatus === "completed";

  // Show confirmation screen before starting
  if (!hasStarted) {
    return (
      <div className="step-content">
        <div className="step-icon">D</div>
        <h3>Generate Docker Configuration</h3>
        <p>
          No <code>docker-compose.yml</code> was found in your <code>noctum/</code> directory.
        </p>

        <div className="noctum-overview">
          <p>The AI will analyze your codebase to create a Docker configuration:</p>
          <ul className="feature-list">
            <li>Detect project type and dependencies</li>
            <li>Configure services and networking</li>
            <li>Set up sandboxed environment (no external network access)</li>
          </ul>
          <p>You may be asked questions about your project during this process.</p>
        </div>

        <button className="btn btn-primary" onClick={onStart}>
          Generate Configuration
        </button>
      </div>
    );
  }

  // Show agent output while running or after completion
  return (
    <div className="step-content compose-generation">
      <h3>{isCompleted ? "Configuration Generated" : "Generating Docker Configuration"}</h3>
      <p>
        {isCompleted
          ? "Your docker-compose.yml has been created."
          : "Analyzing your codebase and creating a docker-compose.yml file."}
      </p>

      <AgentOutput
        output={agentOutput}
        status={agentStatus}
        onRespondQuestion={onRespondQuestion}
        onRespondPermission={onRespondPermission}
      />

      {isCompleted && (
        <div className="step-actions">
          <button className="btn btn-primary" onClick={onComplete}>
            Continue
          </button>
        </div>
      )}
    </div>
  );
}
