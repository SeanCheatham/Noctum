import type { AgentOutput as OutputItem, AgentStatus } from "../hooks/useAgent";
import { QuestionCard } from "./QuestionCard";
import { PermissionCard } from "./PermissionCard";

interface AgentOutputProps {
  output: OutputItem[];
  status: AgentStatus;
  onRespondQuestion: (questionId: string, answer: string) => void;
  onRespondPermission: (permissionId: string, approved: boolean) => void;
}

export function AgentOutput({
  output,
  status,
  onRespondQuestion,
  onRespondPermission,
}: AgentOutputProps) {
  if (output.length === 0 && status === "idle") {
    return <div className="agent-output-empty">Click "Run" to start the agent</div>;
  }

  return (
    <div className="agent-output">
      {output.map((item, index) => {
        if (item.type === "question" && item.questionId) {
          return (
            <QuestionCard
              key={index}
              questionId={item.questionId}
              question={item.content}
              options={item.options}
              status={item.status as "pending" | "answered"}
              onRespond={onRespondQuestion}
            />
          );
        }

        if (item.type === "permission" && item.permissionId) {
          return (
            <PermissionCard
              key={index}
              permissionId={item.permissionId}
              description={item.content}
              command={item.command}
              status={item.status as "pending" | "approved" | "denied"}
              onRespond={onRespondPermission}
            />
          );
        }

        return (
          <div key={index} className={`output-item output-${item.type}`}>
            {item.type === "tool" && <span className="output-badge">Tool</span>}
            {item.type === "error" && <span className="output-badge error">Error</span>}
            <pre>{item.content}</pre>
          </div>
        );
      })}
      {(status === "running" || status === "waiting_for_input") && (
        <div className="output-item output-loading">
          <span className="spinner" />{" "}
          {status === "waiting_for_input" ? "Waiting for your response..." : "Agent is working..."}
        </div>
      )}
    </div>
  );
}
