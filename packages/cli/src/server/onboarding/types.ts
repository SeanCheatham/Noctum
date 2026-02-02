export interface GitStatusResult {
  isClean: boolean;
  branch: string;
  changedFiles: string[];
}

export interface DirectoryCheckResult {
  exists: boolean;
}

export interface ComposeCheckResult {
  exists: boolean;
  path: string;
}

export type OnboardingStep =
  | "git-status"
  | "directory-check"
  | "compose-check"
  | "compose-generation"
  | "ready";

export interface OnboardingState {
  step: OnboardingStep;
  gitStatus: GitStatusResult | null;
  directoryStatus: DirectoryCheckResult | null;
  composeStatus: ComposeCheckResult | null;
}
