//! Search goals and limits for resource use.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Goal {
    #[default]
    Cpu,
    Memory,
    Balanced,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Selection {
    #[default]
    Hot,
    Sweep,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryMetric {
    Allocations,
    #[default]
    AllocatedBytes,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OptimizationConfig {
    pub goal: Goal,
    pub selection: Selection,
    pub memory_metric: MemoryMetric,
    pub min_memory_delta: u64,
    pub min_memory_ratio: f64,
    pub max_cpu_increase_ratio: f64,
    pub max_memory_increase_ratio: f64,
}

impl Default for OptimizationConfig {
    fn default() -> Self {
        Self {
            goal: Goal::Cpu,
            selection: Selection::Hot,
            memory_metric: MemoryMetric::AllocatedBytes,
            min_memory_delta: 1,
            min_memory_ratio: 0.01,
            max_cpu_increase_ratio: 0.0,
            max_memory_increase_ratio: 0.0,
        }
    }
}

impl OptimizationConfig {
    pub fn measures_memory(&self) -> bool {
        self.goal != Goal::Cpu
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        for (name, value) in [
            ("min_memory_ratio", self.min_memory_ratio),
            ("max_cpu_increase_ratio", self.max_cpu_increase_ratio),
            ("max_memory_increase_ratio", self.max_memory_increase_ratio),
        ] {
            anyhow::ensure!(
                value.is_finite() && (0.0..=1.0).contains(&value),
                "{name} must be from 0 to 1"
            );
        }
        anyhow::ensure!(
            self.min_memory_delta > 0,
            "min_memory_delta must be at least 1"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryStats {
    pub allocations: u64,
    pub allocated_bytes: u64,
}

impl MemoryStats {
    pub fn value(self, metric: MemoryMetric) -> u64 {
        match metric {
            MemoryMetric::Allocations => self.allocations,
            MemoryMetric::AllocatedBytes => self.allocated_bytes,
        }
    }
}

/// Read one allocator record. Keep all application output bytes.
pub fn extract_memory(stderr: &[u8]) -> anyhow::Result<(Vec<u8>, Option<MemoryStats>)> {
    const PREFIX: &[u8] = b"\x1eNOCTUM_MEMORY_V1 ";
    let mut output = Vec::with_capacity(stderr.len());
    let mut stats = None;
    let mut offset = 0;
    while let Some(start) = stderr[offset..]
        .windows(PREFIX.len())
        .position(|part| part == PREFIX)
    {
        let start = offset + start;
        output.extend_from_slice(&stderr[offset..start]);
        let end = stderr[start..]
            .iter()
            .position(|b| *b == b'\n')
            .map(|n| start + n)
            .ok_or_else(|| anyhow::anyhow!("incomplete memory record"))?;
        anyhow::ensure!(stats.is_none(), "multiple memory records");
        let record = std::str::from_utf8(&stderr[start + PREFIX.len()..end])?;
        let fields: Vec<_> = record.split_whitespace().collect();
        anyhow::ensure!(fields.len() == 2, "invalid memory record");
        stats = Some(MemoryStats {
            allocations: fields[0].parse()?,
            allocated_bytes: fields[1].parse()?,
        });
        offset = end + 1;
    }
    output.extend_from_slice(&stderr[offset..]);
    Ok((output, stats))
}

pub fn memory_text(json: Option<&str>) -> String {
    json.and_then(|text| serde_json::from_str::<MemoryStats>(text).ok())
        .map(|stats| {
            format!(
                "{} allocations, {} allocated bytes",
                stats.allocations, stats.allocated_bytes
            )
        })
        .unwrap_or_else(|| "not measured".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_keep_raw_application_bytes() {
        let (output, stats) =
            extract_memory(b"\xfftext\x1eNOCTUM_MEMORY_V1 2 400\nmore\0").unwrap();
        assert_eq!(output, b"\xfftextmore\0");
        assert_eq!(
            stats,
            Some(MemoryStats {
                allocations: 2,
                allocated_bytes: 400
            })
        );
        assert!(extract_memory(b"\x1eNOCTUM_MEMORY_V1 2 4\n\x1eNOCTUM_MEMORY_V1 2 4\n").is_err());
        assert!(extract_memory(b"\x1eNOCTUM_MEMORY_V1 2").is_err());
        assert_eq!(
            extract_memory(b"ordinary stderr").unwrap(),
            (b"ordinary stderr".to_vec(), None)
        );
    }

    #[test]
    fn limits_reject_invalid_numbers() {
        for value in [f64::NAN, f64::INFINITY, -0.1, 1.1] {
            let config = OptimizationConfig {
                max_cpu_increase_ratio: value,
                ..Default::default()
            };
            assert!(config.validate().is_err());
        }
    }
}
