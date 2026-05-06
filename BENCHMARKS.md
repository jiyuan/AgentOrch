# Agent OS Benchmarks

Target loop overhead: <= 2 ms per turn excluding LLM and tool latency.

No benchmarks have been run yet. The local environment used for the initial scaffold does not have `cargo` or `rustc` installed, so measurement starts after the Rust toolchain is available.

Planned benchmark coverage:

- Happy-path reply turn.
- Tool-call turn with approve allowed.
- Pause and resume serialization overhead.
- 1000 concurrent conversations for tail latency.
