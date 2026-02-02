import type { OnboardingState, OnboardingStep } from "./types.js";

const initialState: OnboardingState = {
  step: "git-status",
  gitStatus: null,
  directoryStatus: null,
  composeStatus: null,
};

class OnboardingStateStore {
  private state: OnboardingState = { ...initialState };

  getState(): OnboardingState {
    return { ...this.state };
  }

  setStep(step: OnboardingStep): void {
    this.state.step = step;
  }

  updateState(partial: Partial<OnboardingState>): void {
    this.state = { ...this.state, ...partial };
  }

  reset(): void {
    this.state = { ...initialState };
  }
}

export const onboardingState = new OnboardingStateStore();
