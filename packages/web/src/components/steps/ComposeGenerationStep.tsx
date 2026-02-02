import { useEffect } from "react";
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

  useEffect(() => {
    if (!hasStarted) {
      onStart();
    }
  }, [hasStarted, onStart]);

  useEffect(() => {
    if (agentStatus === "completed") {
      onComplete();
    }
  }, [agentStatus, onComplete]);

  return (
    <div className="step-content compose-generation">
      <h3>Generating Docker Configuration</h3>
      <p>Analyzing your codebase and creating a docker-compose.yml file.</p>

      <AgentOutput
        output={agentOutput}
        status={agentStatus}
        onRespondQuestion={onRespondQuestion}
        onRespondPermission={onRespondPermission}
      />
    </div>
  );
}
