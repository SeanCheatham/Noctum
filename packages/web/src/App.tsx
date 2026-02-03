import { useCallback, useEffect, useState } from "react";
import { OnboardingWizard } from "./components/OnboardingWizard";
import { useAgent } from "./hooks/useAgent";
import { useOnboarding } from "./hooks/useOnboarding";

interface HealthData {
  status: string;
  version: string;
  uptime: number;
}

interface ApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

export default function App() {
  const [health, setHealth] = useState<HealthData | null>(null);
  const [error, setError] = useState<string | null>(null);
  const { status, output, run, clear, respondToQuestion, respondToPermission } = useAgent();
  const onboarding = useOnboarding();

  useEffect(() => {
    async function fetchHealth() {
      try {
        const res = await fetch("/api/health");
        const json: ApiResponse<HealthData> = await res.json();
        if (json.success && json.data) {
          setHealth(json.data);
        } else {
          setError(json.error || "Unknown error");
        }
      } catch (err) {
        setError(err instanceof Error ? err.message : "Failed to fetch");
      }
    }

    fetchHealth();
    const interval = setInterval(fetchHealth, 5000);
    return () => clearInterval(interval);
  }, []);

  const handleStartComposeGeneration = useCallback(() => {
    run({ workflow: "compose-generation" });
  }, [run]);

  const handleComposeComplete = useCallback(() => {
    clear();
    onboarding.markComposeComplete();
  }, [clear, onboarding]);

  const handleRun = useCallback(() => {
    run({ workflow: "run-containers" });
  }, [run]);

  const handleReset = useCallback(() => {
    clear();
    onboarding.reset();
  }, [clear, onboarding]);

  const isRunning = status === "running" || status === "waiting_for_input";

  return (
    <main className="container">
      <div className="top-bar">
        <div className="top-bar-brand">
          <h1>Noctum</h1>
          <p className="subtitle">AI-powered sandboxed simulation testing</p>
        </div>
        <div className="top-bar-status">
          {error ? (
            <span className="error">{error}</span>
          ) : health ? (
            <>
              <div className="top-bar-status-item">
                <span className="label">Status</span>
                <span className="value status-ok">{health.status}</span>
              </div>
              <div className="top-bar-status-item">
                <span className="label">v{health.version}</span>
              </div>
              <div className="top-bar-status-item">
                <span className="label">Uptime</span>
                <span className="value">{health.uptime}s</span>
              </div>
            </>
          ) : (
            <span className="top-bar-status-item">Loading...</span>
          )}
        </div>
      </div>

      <div className="agent-card">
        <div className="agent-header">
          <h2>Simulation Setup</h2>
          <div className="agent-controls">
            <button className="btn btn-secondary" onClick={handleReset} disabled={isRunning}>
              Reset
            </button>
          </div>
        </div>

        <OnboardingWizard
          state={onboarding.state}
          onCheckGitStatus={onboarding.checkGitStatus}
          onProceedFromGitWarning={onboarding.proceedFromGitWarning}
          onCheckDirectory={onboarding.checkDirectory}
          onCreateDirectory={onboarding.createDirectoryAndProceed}
          onCheckCompose={onboarding.checkCompose}
          onStartComposeGeneration={handleStartComposeGeneration}
          onComposeComplete={handleComposeComplete}
          onStartRun={handleRun}
          agentOutput={output}
          agentStatus={status}
          onRespondQuestion={respondToQuestion}
          onRespondPermission={respondToPermission}
        />
      </div>
    </main>
  );
}
