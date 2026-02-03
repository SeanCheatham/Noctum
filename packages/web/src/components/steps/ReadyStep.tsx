import { AgentOutput } from "../AgentOutput";
import type { AgentOutput as AgentOutputType, AgentStatus } from "../../hooks/useAgent";

interface RunStepProps {
  agentOutput: AgentOutputType[];
  agentStatus: AgentStatus;
  onStart: () => void;
  onRespondQuestion: (questionId: string, answer: string) => Promise<void>;
  onRespondPermission: (permissionId: string, approved: boolean) => Promise<void>;
}

export function RunStep({
  agentOutput,
  agentStatus,
  onStart,
  onRespondQuestion,
  onRespondPermission,
}: RunStepProps) {
  const hasStarted = agentStatus !== "idle";
  const isCompleted = agentStatus === "completed";
  const isError = agentStatus === "error";

  if (!hasStarted) {
    return (
      <div className="step-content">
        <div className="step-icon success">&#x2713;</div>
        <h3>Ready to Run</h3>
        <p>Your simulation environment is configured and ready to go.</p>

        <div className="noctum-overview">
          <p>The simulation will:</p>
          <ul className="feature-list">
            <li>Build and launch Docker containers</li>
            <li>Run for up to 2 minutes or until exit</li>
            <li>Capture and analyze all output</li>
            <li>Generate a detailed report</li>
          </ul>
        </div>

        <button className="btn btn-primary btn-large" onClick={onStart}>
          Start Simulation
        </button>
      </div>
    );
  }

  const getTitle = () => {
    if (isCompleted) return "Simulation Complete";
    if (isError) return "Simulation Failed";
    return "Running Simulation";
  };

  const getDescription = () => {
    if (isCompleted) return "The simulation has finished. Review the results below.";
    if (isError) return "An error occurred during the simulation.";
    return "Building and running containers...";
  };

  return (
    <div className="step-content run-step">
      <h3>{getTitle()}</h3>
      <p>{getDescription()}</p>

      <AgentOutput
        output={agentOutput}
        status={agentStatus}
        onRespondQuestion={onRespondQuestion}
        onRespondPermission={onRespondPermission}
      />
    </div>
  );
}
