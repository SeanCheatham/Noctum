use sysinfo::System;
use tracing::debug;

/// Monitors system for user idle state
pub struct IdleMonitor {
    threshold_seconds: u64,
    system: System,
}

impl IdleMonitor {
    /// Create a new idle monitor
    pub fn new(threshold_seconds: u64) -> Self {
        Self {
            threshold_seconds,
            system: System::new_all(),
        }
    }

    /// Check if the system is considered idle
    ///
    /// Currently uses a simple heuristic based on CPU usage.
    /// Future versions will use platform-specific APIs for actual user idle time.
    pub fn is_idle(&mut self) -> bool {
        self.system.refresh_cpu_all();

        // Get average CPU usage across all cores
        let cpu_usage: f32 = self.system.cpus().iter().map(|cpu| cpu.cpu_usage()).sum::<f32>()
            / self.system.cpus().len() as f32;

        debug!("Current CPU usage: {:.1}%", cpu_usage);

        // Consider system idle if CPU usage is below 15%
        // This is a simple heuristic - real idle detection would use
        // platform-specific APIs to check actual user input idle time
        cpu_usage < 15.0
    }

    /// Get the idle threshold in seconds
    pub fn threshold(&self) -> u64 {
        self.threshold_seconds
    }
}

#[cfg(target_os = "macos")]
mod platform {
    //! macOS-specific idle detection
    //!
    //! Future implementation will use IOKit to get actual user idle time:
    //! - `CGEventSourceSecondsSinceLastEventType`
    //! - HID idle time from IOKit

    pub fn get_user_idle_seconds() -> Option<u64> {
        // TODO: Implement using IOKit
        None
    }
}

#[cfg(target_os = "linux")]
mod platform {
    //! Linux-specific idle detection
    //!
    //! Future implementation will use:
    //! - X11: `XScreenSaverQueryInfo` for X11 sessions
    //! - Wayland: idle-inhibit protocol or logind
    //! - `/proc/uptime` as fallback

    pub fn get_user_idle_seconds() -> Option<u64> {
        // TODO: Implement using X11/Wayland APIs
        None
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    pub fn get_user_idle_seconds() -> Option<u64> {
        None
    }
}
