interface ReadyStepProps {
  onRun: () => void;
}

export function ReadyStep({ onRun }: ReadyStepProps) {
  return (
    <div className="step-content">
      <div className="step-icon success">&#10003;</div>
      <h3>Ready to Run</h3>
      <p>Your simulation environment is configured and ready.</p>

      <button className="btn btn-primary btn-large" onClick={onRun}>
        Run Simulation
      </button>
    </div>
  );
}
