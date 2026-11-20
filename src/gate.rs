//! Accept or reject one trial. Wall-clock time is not an input.

#[derive(Debug, Clone, PartialEq)]
pub struct GateInput {
    pub baseline: u64,
    pub candidate: u64,
    /// Baseline repeats matched each other, and candidate repeats matched each other.
    pub repeats_match: bool,
    pub guest_ok: bool,
    pub host_ok: bool,
    pub stdout_matches: bool,
    pub diff_ok: bool,
    pub min_improvement_ratio: f64,
    pub min_insn_delta: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GateDecision {
    Accept { improvement: f64, delta: u64 },
    Reject { reason: &'static str },
}

pub fn decide(input: &GateInput) -> GateDecision {
    if !input.diff_ok {
        return GateDecision::Reject {
            reason: "diff rejected",
        };
    }
    if !input.repeats_match {
        return GateDecision::Reject {
            reason: "nondeterministic",
        };
    }
    if !input.guest_ok {
        return GateDecision::Reject {
            reason: "guest failed",
        };
    }
    if !input.host_ok {
        return GateDecision::Reject {
            reason: "host check failed",
        };
    }
    if !input.stdout_matches {
        return GateDecision::Reject {
            reason: "stdout mismatch",
        };
    }
    if input.baseline < input.min_insn_delta {
        return GateDecision::Reject {
            reason: "not eligible",
        };
    }
    if input.candidate == input.baseline {
        return GateDecision::Reject {
            reason: "no change",
        };
    }
    if input.candidate > input.baseline {
        return GateDecision::Reject { reason: "slower" };
    }
    let delta = input.baseline - input.candidate;
    let improvement = delta as f64 / input.baseline as f64;
    if improvement < input.min_improvement_ratio {
        return GateDecision::Reject {
            reason: "below ratio",
        };
    }
    if delta < input.min_insn_delta {
        return GateDecision::Reject {
            reason: "below absolute delta",
        };
    }
    GateDecision::Accept { improvement, delta }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_input() -> GateInput {
        GateInput {
            baseline: 100_000,
            candidate: 80_000,
            repeats_match: true,
            guest_ok: true,
            host_ok: true,
            stdout_matches: true,
            diff_ok: true,
            min_improvement_ratio: 0.01,
            min_insn_delta: 10_000,
        }
    }

    #[test]
    fn accepts_a_real_win() {
        let decision = decide(&ok_input());
        assert_eq!(
            decision,
            GateDecision::Accept {
                improvement: 0.2,
                delta: 20_000,
            }
        );
    }

    #[test]
    fn rejects_each_failed_check() {
        let cases = [
            ("diff_ok", "diff rejected"),
            ("repeats_match", "nondeterministic"),
            ("guest_ok", "guest failed"),
            ("host_ok", "host check failed"),
            ("stdout_matches", "stdout mismatch"),
        ];
        for (field, reason) in cases {
            let mut input = ok_input();
            match field {
                "diff_ok" => input.diff_ok = false,
                "repeats_match" => input.repeats_match = false,
                "guest_ok" => input.guest_ok = false,
                "host_ok" => input.host_ok = false,
                "stdout_matches" => input.stdout_matches = false,
                _ => unreachable!(),
            }
            assert_eq!(decide(&input), GateDecision::Reject { reason }, "{field}");
        }
    }

    #[test]
    fn rejects_no_change_and_a_slowdown() {
        let mut same = ok_input();
        same.candidate = same.baseline;
        assert_eq!(
            decide(&same),
            GateDecision::Reject {
                reason: "no change"
            }
        );

        let mut slower = ok_input();
        slower.candidate = 120_000;
        assert_eq!(decide(&slower), GateDecision::Reject { reason: "slower" });
    }

    #[test]
    fn rejects_a_tiny_body_and_a_small_win() {
        let mut tiny = ok_input();
        tiny.baseline = 9_999;
        tiny.candidate = 1;
        assert_eq!(
            decide(&tiny),
            GateDecision::Reject {
                reason: "not eligible"
            }
        );

        let mut ratio = ok_input();
        ratio.baseline = 2_000_000;
        ratio.candidate = 1_989_000;
        assert_eq!(
            decide(&ratio),
            GateDecision::Reject {
                reason: "below ratio"
            }
        );

        let mut delta = ok_input();
        delta.baseline = 20_000;
        delta.candidate = 15_000;
        assert_eq!(
            decide(&delta),
            GateDecision::Reject {
                reason: "below absolute delta"
            }
        );
    }
}
