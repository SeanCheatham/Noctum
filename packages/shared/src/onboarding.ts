export type OnboardingStep =
  | "git-status"
  | "directory-check"
  | "compose-check"
  | "compose-generation"
  | "ready";

export interface GitStatusData {
  isClean: boolean;
  branch: string;
  changedFiles: string[];
}

export interface DirectoryCheckData {
  exists: boolean;
}

export interface ComposeCheckData {
  exists: boolean;
  path: string;
}

export interface OnboardingState {
  step: OnboardingStep;
  gitStatus: GitStatusData | null;
  directoryStatus: DirectoryCheckData | null;
  composeStatus: ComposeCheckData | null;
}
