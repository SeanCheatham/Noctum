import type { OnboardingState as BaseOnboardingState } from "@noctum/shared";

export type {
  OnboardingStep,
  GitStatusData,
  DirectoryCheckData,
  ComposeCheckData,
} from "@noctum/shared";

// Frontend-specific extension with loading and error state
export interface OnboardingState extends BaseOnboardingState {
  isLoading: boolean;
  error: string | null;
}
