import { useEffect } from "react";
import type { GitStatusData } from "../../types/onboarding";

interface GitStatusStepProps {
  data: GitStatusData | null;
  isLoading: boolean;
  onCheck: () => void;
  onProceed: () => void;
}

export function GitStatusStep({ data, isLoading, onCheck, onProceed }: GitStatusStepProps) {
  useEffect(() => {
    if (!data && !isLoading) {
      onCheck();
    }
  }, [data, isLoading, onCheck]);

  if (isLoading) {
    return (
      <div className="step-loading">
        <span className="spinner" /> Checking git status...
      </div>
    );
  }

  if (!data) {
    return null;
  }

  if (data.isClean) {
    return (
      <div className="step-loading">
        <span className="spinner" /> Git status clean, continuing...
      </div>
    );
  }

  return (
    <div className="step-content">
      <div className="step-icon warning">!</div>
      <h3>Uncommitted Changes Detected</h3>
      <p>
        Your working directory has uncommitted changes on branch <code>{data.branch}</code>.
      </p>

      <div className="changed-files">
        <p>{data.changedFiles.length} file(s) modified:</p>
        <ul>
          {data.changedFiles.slice(0, 5).map((file) => (
            <li key={file}>{file}</li>
          ))}
          {data.changedFiles.length > 5 && (
            <li className="more">...and {data.changedFiles.length - 5} more</li>
          )}
        </ul>
      </div>

      <p className="step-warning">
        Noctum may create or modify files in your project. Consider committing your changes first.
      </p>

      <button className="btn btn-primary" onClick={onProceed}>
        Proceed Anyway
      </button>
    </div>
  );
}
