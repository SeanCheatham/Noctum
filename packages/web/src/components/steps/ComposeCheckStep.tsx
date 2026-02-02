import { useEffect } from "react";
import type { ComposeCheckData } from "../../types/onboarding";

interface ComposeCheckStepProps {
  data: ComposeCheckData | null;
  isLoading: boolean;
  onCheck: () => void;
}

export function ComposeCheckStep({ data, isLoading, onCheck }: ComposeCheckStepProps) {
  useEffect(() => {
    if (!data && !isLoading) {
      onCheck();
    }
  }, [data, isLoading, onCheck]);

  return (
    <div className="step-loading">
      <span className="spinner" /> Checking docker-compose configuration...
    </div>
  );
}
