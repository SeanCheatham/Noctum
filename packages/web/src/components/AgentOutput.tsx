import type { AgentOutput as OutputItem, AgentStatus } from "../hooks/useAgent";

interface AgentOutputProps {
  output: OutputItem[];
  status: AgentStatus;
}

export function AgentOutput({ output, status }: AgentOutputProps) {
  if (output.length === 0 && status === "idle") {
    return <div className="agent-output-empty">Click "Run" to start the agent</div>;
  }

  return (
    <div className="agent-output">
      {output.map((item, index) => (
        <div key={index} className={`output-item output-${item.type}`}>
          {item.type === "tool" && <span className="output-badge">Tool</span>}
          {item.type === "error" && <span className="output-badge error">Error</span>}
          <pre>{item.content}</pre>
        </div>
      ))}
      {status === "running" && (
        <div className="output-item output-loading">
          <span className="spinner" /> Agent is working...
        </div>
      )}
    </div>
  );
}
