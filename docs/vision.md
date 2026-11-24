# Project direction

Noctum searches Rust functions for useful CPU and memory reductions. It uses static findings to select experiments and measured workloads to decide whether to keep changes. Each run preserves the user's source and records the evidence needed to review accepted changes.

The search resumes across nights. Functions without representative workloads remain visible but cannot produce accepted changes. Future work can add typed analysis, generated comparison tests, peak heap measurements, and changes across related functions.
