import { useEffect } from "react";
import type { DirectoryCheckData } from "../../types/onboarding";

interface DirectoryCheckStepProps {
  data: DirectoryCheckData | null;
  isLoading: boolean;
  onCheck: () => void;
  onCreateAndContinue: () => void;
}

export function DirectoryCheckStep({
  data,
  isLoading,
  onCheck,
  onCreateAndContinue,
}: DirectoryCheckStepProps) {
  useEffect(() => {
    if (!data && !isLoading) {
      onCheck();
    }
  }, [data, isLoading, onCheck]);

  if (isLoading) {
    return (
      <div className="step-loading">
        <span className="spinner" /> Checking noctum directory...
      </div>
    );
  }

  if (!data) {
    return null;
  }

  if (data.exists) {
    return (
      <div className="step-loading">
        <span className="spinner" /> Directory found, continuing...
      </div>
    );
  }

  return (
    <div className="step-content">
      <div className="step-icon">&gt;_</div>
      <h3>Welcome to Noctum</h3>

      <div className="noctum-overview">
        <p>Test your application in a safe, isolated Docker environment.</p>

        <ul className="feature-list">
          <li>Sandboxed execution with no network access</li>
          <li>AI-powered docker-compose generation</li>
          <li>Automatic dependency detection</li>
          <li>Real-time log analysis and reporting</li>
        </ul>

        <p>
          A <code>noctum/</code> directory will be created for configuration.
        </p>
      </div>

      <button className="btn btn-primary btn-large" onClick={onCreateAndContinue}>
        Get Started
      </button>
    </div>
  );
}
