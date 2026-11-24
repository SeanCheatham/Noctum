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
    pub stderr_matches: bool,
    pub diff_ok: bool,
    pub min_improvement_ratio: f64,
    pub min_insn_delta: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GateDecision {
    Accept { improvement: f64, delta: u64 },
    Reject { reason: &'static str },
}

fn checks(input: &GateInput) -> Option<GateDecision> {
    if !input.diff_ok {
        return Some(GateDecision::Reject {
            reason: "diff rejected",
        });
    }
    if !input.repeats_match {
        return Some(GateDecision::Reject {
            reason: "nondeterministic",
        });
    }
    if !input.guest_ok {
        return Some(GateDecision::Reject {
            reason: "guest failed",
        });
    }
    if !input.host_ok {
        return Some(GateDecision::Reject {
            reason: "host check failed",
        });
    }
    if !input.stdout_matches {
        return Some(GateDecision::Reject {
            reason: "stdout mismatch",
        });
    }
    if !input.stderr_matches {
        return Some(GateDecision::Reject {
            reason: "stderr mismatch",
        });
    }
    None
}

pub fn decide(input: &GateInput) -> GateDecision {
    if let Some(decision) = checks(input) {
        return decision;
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

pub fn decide_resources(
    input: &GateInput,
    config: &crate::optimization::OptimizationConfig,
    baseline: Option<crate::optimization::MemoryStats>,
    candidate: Option<crate::optimization::MemoryStats>,
) -> GateDecision {
    use crate::optimization::Goal;
    if let Some(decision) = checks(input) {
        return decision;
    }
    if config.goal == Goal::Cpu {
        return decide(input);
    }
    let (Some(before), Some(after)) = (baseline, candidate) else {
        return GateDecision::Reject {
            reason: "missing memory measurement",
        };
    };
    let within = |old: u64, new: u64, limit: f64| {
        new <= old || (old > 0 && (new - old) as f64 / old as f64 <= limit)
    };
    if !within(
        input.baseline,
        input.candidate,
        config.max_cpu_increase_ratio,
    ) {
        return GateDecision::Reject {
            reason: "instruction increase exceeds limit",
        };
    }
    if !within(
        before.allocations,
        after.allocations,
        config.max_memory_increase_ratio,
    ) || !within(
        before.allocated_bytes,
        after.allocated_bytes,
        config.max_memory_increase_ratio,
    ) {
        return GateDecision::Reject {
            reason: "memory increase exceeds limit",
        };
    }
    let old = before.value(config.memory_metric);
    let new = after.value(config.memory_metric);
    let memory_win = old > new
        && old - new >= config.min_memory_delta
        && (old - new) as f64 / old as f64 >= config.min_memory_ratio;
    let cpu_win = matches!(decide(input), GateDecision::Accept { .. });
    if !memory_win && !(config.goal == Goal::Balanced && cpu_win) {
        return GateDecision::Reject {
            reason: "below memory improvement floor",
        };
    }
    GateDecision::Accept {
        improvement: if input.baseline == 0 {
            0.0
        } else {
            (input.baseline as f64 - input.candidate as f64) / input.baseline as f64
        },
        delta: input.baseline.saturating_sub(input.candidate),
    }
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
            stderr_matches: true,
            diff_ok: true,
            min_improvement_ratio: 0.01,
            min_insn_delta: 10_000,
        }
    }

    #[test]
    fn memory_goals_enforce_both_resource_limits() {
        use crate::optimization::{Goal, MemoryMetric, MemoryStats, OptimizationConfig};
        let stats = |allocations, allocated_bytes| {
            Some(MemoryStats {
                allocations,
                allocated_bytes,
            })
        };
        let mut input = ok_input();
        input.candidate = input.baseline;
        let mut config = OptimizationConfig {
            goal: Goal::Memory,
            ..Default::default()
        };
        assert!(matches!(
            decide_resources(&input, &config, stats(10, 1000), stats(10, 500)),
            GateDecision::Accept { .. }
        ));
        assert_eq!(
            decide_resources(&input, &config, None, stats(10, 500)),
            GateDecision::Reject {
                reason: "missing memory measurement"
            }
        );
        assert_eq!(
            decide_resources(&input, &config, stats(10, 1000), stats(11, 500)),
            GateDecision::Reject {
                reason: "memory increase exceeds limit"
            }
        );
        input.candidate += 100;
        assert_eq!(
            decide_resources(&input, &config, stats(10, 1000), stats(10, 500)),
            GateDecision::Reject {
                reason: "instruction increase exceeds limit"
            }
        );
        config.max_cpu_increase_ratio = 0.01;
        assert!(matches!(
            decide_resources(&input, &config, stats(10, 1000), stats(10, 500)),
            GateDecision::Accept { .. }
        ));
        config.memory_metric = MemoryMetric::Allocations;
        assert!(matches!(
            decide_resources(&input, &config, stats(10, 1000), stats(9, 1000)),
            GateDecision::Accept { .. }
        ));
        assert!(matches!(
            decide_resources(&input, &config, stats(10, 1000), stats(10, 500)),
            GateDecision::Reject { .. }
        ));
    }

    #[test]
    fn balanced_accepts_either_win_and_keeps_correctness_checks() {
        use crate::optimization::{Goal, MemoryStats, OptimizationConfig};
        let config = OptimizationConfig {
            goal: Goal::Balanced,
            ..Default::default()
        };
        let stats = Some(MemoryStats {
            allocations: 10,
            allocated_bytes: 1000,
        });
        let mut input = ok_input();
        assert!(matches!(
            decide_resources(&input, &config, stats, stats),
            GateDecision::Accept { .. }
        ));
        input.candidate = input.baseline;
        assert!(matches!(
            decide_resources(&input, &config, stats, stats),
            GateDecision::Reject { .. }
        ));
        input.stdout_matches = false;
        assert_eq!(
            decide_resources(
                &input,
                &config,
                stats,
                Some(MemoryStats {
                    allocations: 1,
                    allocated_bytes: 1
                })
            ),
            GateDecision::Reject {
                reason: "stdout mismatch"
            }
        );
        input.stdout_matches = true;
        input.baseline = 0;
        input.candidate = 1;
        assert!(matches!(
            decide_resources(&input, &config, stats, stats),
            GateDecision::Reject { .. }
        ));
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
            ("stderr_matches", "stderr mismatch"),
        ];
        for (field, reason) in cases {
            let mut input = ok_input();
            match field {
                "diff_ok" => input.diff_ok = false,
                "repeats_match" => input.repeats_match = false,
                "guest_ok" => input.guest_ok = false,
                "host_ok" => input.host_ok = false,
                "stdout_matches" => input.stdout_matches = false,
                "stderr_matches" => input.stderr_matches = false,
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
