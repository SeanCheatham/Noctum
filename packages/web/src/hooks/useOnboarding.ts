import { useCallback, useEffect, useReducer } from "react";
import type {
  ApiResponse,
  OnboardingState as BackendOnboardingState,
  OnboardingStep,
  GitStatusData,
  DirectoryCheckData,
  ComposeCheckData,
} from "@noctum/shared";
import type { OnboardingState } from "../types/onboarding";

type OnboardingAction =
  | { type: "START_CHECK" }
  | { type: "SYNC_STATE"; data: BackendOnboardingState }
  | { type: "GIT_STATUS_COMPLETE"; data: GitStatusData }
  | { type: "DIRECTORY_CHECK_COMPLETE"; data: DirectoryCheckData }
  | { type: "COMPOSE_CHECK_COMPLETE"; data: ComposeCheckData }
  | { type: "SET_STEP"; step: OnboardingStep }
  | { type: "SET_ERROR"; error: string }
  | { type: "RESET" };

const initialState: OnboardingState = {
  step: "git-status",
  gitStatus: null,
  directoryStatus: null,
  composeStatus: null,
  isLoading: true,
  error: null,
};

function reducer(state: OnboardingState, action: OnboardingAction): OnboardingState {
  switch (action.type) {
    case "START_CHECK":
      return { ...state, isLoading: true, error: null };
    case "SYNC_STATE":
      return {
        ...state,
        step: action.data.step,
        gitStatus: action.data.gitStatus,
        directoryStatus: action.data.directoryStatus,
        composeStatus: action.data.composeStatus,
        isLoading: false,
      };
    case "GIT_STATUS_COMPLETE":
      return {
        ...state,
        gitStatus: action.data,
        isLoading: false,
        step: action.data.isClean ? "directory-check" : "git-status",
      };
    case "DIRECTORY_CHECK_COMPLETE":
      return {
        ...state,
        directoryStatus: action.data,
        isLoading: false,
        step: action.data.exists ? "compose-check" : "directory-check",
      };
    case "COMPOSE_CHECK_COMPLETE":
      return {
        ...state,
        composeStatus: action.data,
        isLoading: false,
        step: action.data.exists ? "ready" : "compose-generation",
      };
    case "SET_STEP":
      return { ...state, step: action.step, isLoading: false };
    case "SET_ERROR":
      return { ...state, error: action.error, isLoading: false };
    case "RESET":
      return { ...initialState, isLoading: false };
    default:
      return state;
  }
}

export function useOnboarding() {
  const [state, dispatch] = useReducer(reducer, initialState);

  // Fetch initial state on mount
  useEffect(() => {
    async function fetchState() {
      try {
        const res = await fetch("/api/onboarding/state");
        const json: ApiResponse<BackendOnboardingState> = await res.json();
        if (json.success && json.data) {
          dispatch({ type: "SYNC_STATE", data: json.data });
        } else {
          dispatch({ type: "SET_ERROR", error: json.error || "Failed to fetch state" });
        }
      } catch {
        dispatch({ type: "SET_ERROR", error: "Failed to fetch state" });
      }
    }
    fetchState();
  }, []);

  const checkGitStatus = useCallback(async () => {
    dispatch({ type: "START_CHECK" });
    try {
      const res = await fetch("/api/onboarding/git-status");
      const json: ApiResponse<GitStatusData> = await res.json();
      if (json.success && json.data) {
        dispatch({ type: "GIT_STATUS_COMPLETE", data: json.data });
      } else {
        dispatch({ type: "SET_ERROR", error: json.error || "Failed to check git status" });
      }
    } catch {
      dispatch({ type: "SET_ERROR", error: "Failed to check git status" });
    }
  }, []);

  const proceedFromGitWarning = useCallback(async () => {
    try {
      await fetch("/api/onboarding/step", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ step: "directory-check" }),
      });
    } catch {
      // Ignore backend errors for step setting
    }
    dispatch({ type: "SET_STEP", step: "directory-check" });
  }, []);

  const checkDirectory = useCallback(async () => {
    dispatch({ type: "START_CHECK" });
    try {
      const res = await fetch("/api/onboarding/check-directory");
      const json: ApiResponse<DirectoryCheckData> = await res.json();
      if (json.success && json.data) {
        dispatch({ type: "DIRECTORY_CHECK_COMPLETE", data: json.data });
      } else {
        dispatch({ type: "SET_ERROR", error: json.error || "Failed to check directory" });
      }
    } catch {
      dispatch({ type: "SET_ERROR", error: "Failed to check directory" });
    }
  }, []);

  const createDirectoryAndProceed = useCallback(async () => {
    dispatch({ type: "START_CHECK" });
    try {
      const res = await fetch("/api/onboarding/create-directory", { method: "POST" });
      const json: ApiResponse<void> = await res.json();
      if (json.success) {
        dispatch({ type: "SET_STEP", step: "compose-check" });
      } else {
        dispatch({ type: "SET_ERROR", error: json.error || "Failed to create directory" });
      }
    } catch {
      dispatch({ type: "SET_ERROR", error: "Failed to create directory" });
    }
  }, []);

  const checkCompose = useCallback(async () => {
    dispatch({ type: "START_CHECK" });
    try {
      const res = await fetch("/api/onboarding/check-compose");
      const json: ApiResponse<ComposeCheckData> = await res.json();
      if (json.success && json.data) {
        dispatch({ type: "COMPOSE_CHECK_COMPLETE", data: json.data });
      } else {
        dispatch({ type: "SET_ERROR", error: json.error || "Failed to check compose file" });
      }
    } catch {
      dispatch({ type: "SET_ERROR", error: "Failed to check compose file" });
    }
  }, []);

  const markComposeComplete = useCallback(async () => {
    try {
      await fetch("/api/onboarding/step", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ step: "ready" }),
      });
    } catch {
      // Ignore backend errors for step setting
    }
    dispatch({ type: "SET_STEP", step: "ready" });
  }, []);

  const reset = useCallback(async () => {
    try {
      await fetch("/api/onboarding/reset", { method: "POST" });
    } catch {
      // Ignore reset errors
    }
    dispatch({ type: "RESET" });
  }, []);

  return {
    state,
    checkGitStatus,
    proceedFromGitWarning,
    checkDirectory,
    createDirectoryAndProceed,
    checkCompose,
    markComposeComplete,
    reset,
  };
}
