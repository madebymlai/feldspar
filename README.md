# Feldspar

A cognitive reasoning MCP server for Claude Code. Combines a tailored Sequential Thinking tool with 5 real-time analyzers, auto-warnings, and ML-powered pattern recall into a single Rust binary.

## What It Does

Claude Code calls `sequentialthinking` to reason through problems step by step. Feldspar watches every thought and pushes back when it detects:

- **Overconfidence** -- claims 85% confidence with zero evidence? Calibrator fires.
- **Sycophancy** -- agrees with itself for 3 thoughts without branching? Guard fires.
- **Cognitive bias** -- anchoring, confirmation, sunk cost, availability, overconfidence.
- **Shallow analysis** -- wrapping up a 6-component architecture decision in 2 thoughts? Underthinking warning.
- **Shortcut language** -- "quick fix", "just skip", "pre-existing issue"? Anti-quick-fix warning.

After each trace completes, an external model scores the reasoning quality (0-10 trust score). PerpetualBooster learns which reasoning patterns produce trusted conclusions and warns Claude when it recognizes a failing pattern.

## Architecture

```
Claude Code <-> [HTTP/MCP] <-> Feldspar Rust Binary
                                 +-- Thought Processor (traces, branches, recap)
                                 +-- Warning Engine (regex patterns, mode validation)
                                 +-- Observer/Evaluator Pipeline
                                 |    +-- Observers: depth, bias, budget
                                 |    +-- Evaluators: confidence, sycophancy
                                 +-- PerpetualBooster (predict per thought, train on outcome)
                                 +-- Trace Review (OpenRouter, trust scoring)
                                 +-- SQLite (persistence, pattern recall)
```

Single HTTP daemon per project. Multiple Claude Code sessions connect concurrently. One ML model shared across all sessions, always up to date.

## Key Concepts

- **Trace** -- one reasoning chain, identified by UUID. Multiple traces can run concurrently.
- **Thinking Mode** -- domain-specific validation (architecture, debugging, performance, etc.). Configurable per project.
- **Observers** -- depth, bias, budget analyzers. Run in parallel, produce raw signals.
- **Evaluators** -- confidence calibrator, sycophancy guard. Run in parallel, read observer output.
- **Trust Score** -- external model judges each completed trace (0-10). ML target variable.
- **Pattern Recall** -- PerpetualBooster finds similar past traces and reports what worked/failed.

## Status

Design complete. Implementation in progress.

See `docs/plans/2026-04-06-thought-processor-design.md` for the full design document.

## Setup

```bash
# Build
cargo build --release

# Configure (per project)
# config/feldspar.toml -- thresholds, thinking modes, components

# Run
./target/release/feldspar

# Connect Claude Code
# .mcp.json:
{
  "mcpServers": {
    "feldspar": {
      "type": "http",
      "url": "http://localhost:3581"
    }
  }
}
```

## Inspired By

- [Sequential Thinking MCP](https://github.com/modelcontextprotocol/servers/tree/main/src/sequentialthinking) -- the vanilla base
- [Reddit post](https://www.reddit.com/r/ClaudeCode/comments/1rcvc15/) -- the tailored enhancement idea
- [PerpetualBooster](https://github.com/perpetual-ml/perpetual) -- zero-config ML in Rust
- [EvalAct](https://arxiv.org/abs/2603.09203) -- process rewards for agent self-evaluation

## License

MIT
