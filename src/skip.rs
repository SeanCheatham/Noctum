//! Instructions for tests that cannot be used for optimization.

pub fn explain(status: &str, reason: Option<&str>) -> Option<String> {
    if !status.starts_with("skipped_") {
        return reason.map(str::to_string);
    }
    let reason = reason.unwrap_or(if status == "skipped_small" {
        "instruction count is below the optimization floor"
    } else {
        "test was skipped"
    });
    let help = match (status, reason) {
        ("skipped_small", _) => "The count is repeatable. Use a larger fixed workload if optimization is necessary.",
        ("skipped_nondet", _) => "Use fixed input and a fixed loop count. Remove clocks, random input, and external state from the measured code.",
        (_, "skip_tests") => "Remove the matching skip_tests entry if this test should run.",
        (_, "ignore") => "Select an active test, or remove #[ignore] if the test should run.",
        (_, "should_panic") => "Use a separate test that returns normally.",
        (_, "custom test attribute") => "Use a plain #[test] function for guest measurement.",
        (_, "adaptive bench") => "Use a bench with harness = false, fn main, and a fixed loop count.",
        (_, "bench has no fn main") => "Add fn main to the bench and set harness = false.",
        (_, reason) if reason.starts_with("denylist:") => {
            if ["thread_rng", "rand::rng", "rand::random", "OsRng", "from_entropy", "from_os_rng", "getrandom"].iter().any(|s| reason.contains(s)) {
                "This test uses random input without a fixed seed. Use a seeded generator and a fixed workload."
            } else if ["time", "Instant", "SystemTime", "sleep"].iter().any(|s| reason.contains(s)) {
                "This test uses a clock or delay. Use fixed input and a fixed loop count."
            } else if ["net", "Socket", "Stream", "reqwest", "hyper"].iter().any(|s| reason.contains(s)) {
                "This test uses network access. Test the data processing code with local fixed input."
            } else if reason.contains("process") || reason.contains("Command") {
                "This test starts a process. Test the function directly in the guest."
            } else {
                "This test uses an external service. Test the data processing code with local fixed input."
            }
        }
        (_, reason) if reason.contains("ambiguous") => "Use --test with one of the full test names shown above.",
        ("skipped_fail", _) => "Check the guest output and fence markers. The selected test must pass in a static Linux musl binary.",
        _ => "Check the test configuration and the command log.",
    };
    Some(format!("{reason}. {help}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_and_network_tests_have_different_instructions() {
        let clock = explain("skipped_env", Some("denylist: Instant::now")).unwrap();
        let network = explain("skipped_env", Some("denylist: TcpStream")).unwrap();
        assert!(clock.contains("fixed loop count"));
        assert!(network.contains("network access"));
        assert!(!network.contains("clock"));
        assert!(explain("skipped_small", None)
            .unwrap()
            .contains("repeatable"));
        assert!(explain("measured", None).is_none());
    }
}
