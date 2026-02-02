import type { OnboardingStep } from "../types/onboarding";

interface ProgressIndicatorProps {
  currentStep: OnboardingStep;
}

const STEPS = [
  { key: "git-status", label: "Git Status" },
  { key: "directory-check", label: "Setup" },
  { key: "compose-check", label: "Docker Config" },
  { key: "ready", label: "Run" },
] as const;

export function ProgressIndicator({ currentStep }: ProgressIndicatorProps) {
  const currentIndex = STEPS.findIndex(
    (s) =>
      s.key === currentStep || (currentStep === "compose-generation" && s.key === "compose-check")
  );

  return (
    <div className="progress-indicator">
      {STEPS.map((step, index) => (
        <div
          key={step.key}
          className={`progress-step ${index <= currentIndex ? "active" : ""} ${index < currentIndex ? "completed" : ""}`}
        >
          <div className="step-marker">{index + 1}</div>
          <div className="step-label">{step.label}</div>
        </div>
      ))}
    </div>
  );
}
