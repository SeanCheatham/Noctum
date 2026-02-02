import type { OnboardingState } from "../types/onboarding";
import type { AgentOutput as AgentOutputType, AgentStatus } from "../hooks/useAgent";
import { ProgressIndicator } from "./ProgressIndicator";
import { GitStatusStep } from "./steps/GitStatusStep";
import { DirectoryCheckStep } from "./steps/DirectoryCheckStep";
import { ComposeCheckStep } from "./steps/ComposeCheckStep";
import { ComposeGenerationStep } from "./steps/ComposeGenerationStep";
import { ReadyStep } from "./steps/ReadyStep";

interface OnboardingWizardProps {
  state: OnboardingState;
  onCheckGitStatus: () => void;
  onProceedFromGitWarning: () => void;
  onCheckDirectory: () => void;
  onCreateDirectory: () => void;
  onCheckCompose: () => void;
  onStartComposeGeneration: () => void;
  onComposeComplete: () => void;
  onRun: () => void;
  agentOutput: AgentOutputType[];
  agentStatus: AgentStatus;
  onRespondQuestion: (id: string, answer: string) => Promise<void>;
  onRespondPermission: (id: string, approved: boolean) => Promise<void>;
}

export function OnboardingWizard({
  state,
  onCheckGitStatus,
  onProceedFromGitWarning,
  onCheckDirectory,
  onCreateDirectory,
  onCheckCompose,
  onStartComposeGeneration,
  onComposeComplete,
  onRun,
  agentOutput,
  agentStatus,
  onRespondQuestion,
  onRespondPermission,
}: OnboardingWizardProps) {
  return (
    <div className="onboarding-wizard">
      <div className="onboarding-progress">
        <ProgressIndicator currentStep={state.step} />
      </div>

      <div className="onboarding-content">
        {state.error && <div className="onboarding-error">{state.error}</div>}

        {state.step === "git-status" && (
          <GitStatusStep
            data={state.gitStatus}
            isLoading={state.isLoading}
            onCheck={onCheckGitStatus}
            onProceed={onProceedFromGitWarning}
          />
        )}

        {state.step === "directory-check" && (
          <DirectoryCheckStep
            data={state.directoryStatus}
            isLoading={state.isLoading}
            onCheck={onCheckDirectory}
            onCreateAndContinue={onCreateDirectory}
          />
        )}

        {state.step === "compose-check" && (
          <ComposeCheckStep
            data={state.composeStatus}
            isLoading={state.isLoading}
            onCheck={onCheckCompose}
          />
        )}

        {state.step === "compose-generation" && (
          <ComposeGenerationStep
            agentOutput={agentOutput}
            agentStatus={agentStatus}
            onStart={onStartComposeGeneration}
            onComplete={onComposeComplete}
            onRespondQuestion={onRespondQuestion}
            onRespondPermission={onRespondPermission}
          />
        )}

        {state.step === "ready" && <ReadyStep onRun={onRun} />}
      </div>
    </div>
  );
}
