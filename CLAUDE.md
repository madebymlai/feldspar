# Claude Development Protocol

## 1. Identity & Constraints

**What we're building**: A Claude Code power framework that combines cognitive reasoning (tailored Sequential Thinking MCP with 5 analyzers), real-time ML learning (PerpetualBooster), and battle-tested patterns into a single installable toolkit. Makes Claude Code smarter, self-improving, and opinionated.

### Design Philosophy
- **First Principles**: Understand the system before you reach for abstractions. Know what the framework hides; know what the library costs. Custom solutions beat cargo-culted patterns. If you need a hack, your model is wrong—fix the design. Actively seek what could break it.
- **Spec-Driven**: Design precedes code. No implementation without a validated plan.
- **Test-Driven**: Tests are written *with* the code, not after. Red → Green → Refactor.
- **Atomic Tasks**: Work is broken into small, verifiable units. 10-15 tasks per feature.
- **Verification-First**: High friction ensures high quality.

### Hard Constraints (never violate)

1. **Single Rust binary for MCP+ML core**: No Python bridge, no Node runtime in the critical path. The MCP server and PerpetualBooster live in one process.
2. **Best-effort persistence**: Never block on DB/ML failure. The MCP runs perfectly without persistence. Every fallible I/O is fire-and-forget.
3. **Warnings advisory, not blocking**: Claude can override with justification. No hard gates on reasoning.
4. **Forward-first**: No backward compatibility unless explicitly instructed.
5. **No defensive garbage**: Let bugs surface. No fallbacks for impossible cases. No `else` branches "just in case." Trust contracts -- if something shouldn't happen, let it fail loud.
6. **SOLID + Composition over Inheritance**: DIP everywhere -- depend on abstractions, never concrete implementations. No class hierarchies deeper than 2. Use decorators, aggregators, and delegation.
7. **Make invalid states unrepresentable**: Use Rust's type system. Enums over flags. Discriminated unions over boolean combinations. If it compiles, it's valid.

### Coding Standards

- **SRP**: One module, one reason to change. Ask: "Can I describe this module's purpose without using 'and'?"
- **OCP**: Extend behavior without modifying existing code. If adding a feature requires touching multiple files, the abstraction is wrong.
- **LSP**: Any implementation of a trait must work anywhere that trait is used.
- **ISP**: Small, focused traits. No struct should implement methods it doesn't need.
- **DIP**: Import traits, never concrete implementations. All external I/O through a trait.
- **KISS**: Simplest solution that works. Boring code is good code.
- **DRY**: If you write the same logic twice, extract it.
- **Tell, Don't Ask**: Put behavior where the data lives. Prefer `object.do()` over branching on object internals.

### Architecture Overview

| Layer | Implementation |
|-------|----------------|
| **MCP Core** | Rust (MCP protocol handler, thought processor, session management) |
| **Cognitive Analyzers** | Rust (in-process: depth, confidence, sycophancy, budget, bias) |
| **ML Layer** | Rust (`perpetual` crate -- PerpetualBooster linked as library) |
| **Hooks/Skills/Commands** | TypeScript (Claude Code ecosystem) |
| **Config/Prompts** | TypeScript (type-safe configs, template literals for prompts) |

**Data Flow**:
```
Claude Code ←→ [stdio/MCP] ←→ Feldspar Rust Binary
                                  ├── Thought Processor (store, branch, recap)
                                  ├── Warning Engine (anti-quick-fix, budget, mode validation)
                                  ├── Cognitive Analyzers (5 independent, merged into response)
                                  │   ├── Depth Analyzer
                                  │   ├── Confidence Calibrator
                                  │   ├── Sycophancy Guard
                                  │   ├── Budget Advisor
                                  │   └── Bias Detector
                                  ├── PerpetualBooster (predict per thought, train on session end)
                                  └── Config Loader (TS-compiled configs)
```

---

## 2. Navigation and Toolkit

### Documentation Map

| Module | Path | Purpose |
|--------|------|---------|
| **Rust MCP Core** | `src/` | MCP server, thought processor, analyzers, ML bridge, config |
| **Claude Code Layer** | `.claude/skills/`, `.claude/agents/`, `.claude/resources/` | Skills, agents, style guides |
| **Hooks** | `hooks/hooks.json`, `hooks/scripts/` | Event-driven automations (TS) |
| **Config** | `config/` | Type-safe component maps, thresholds, prompts (TS) |
| **Rules** | `rules/common/`, `rules/rust/` | Coding standards |
| **References** | `_refs/` | Cloned repos, deep dive docs |

### Exploration Pattern

1. Reference `_refs/` for design patterns and prior art
2. Reference `.claude/resources/` for style guides before writing code
3. For Rust core: start with `src/main.rs` → `thought.rs` → `analyzers/mod.rs`
4. For Claude Code layer: start with `.claude/skills/` → `.claude/agents/`
5. For config: `config/thresholds.ts` defines all analyzer parameters

---

## 3. Code Patterns

### Project Structure

```
feldspar/
├── src/                       # Rust MCP core
│   ├── main.rs                # MCP server entry (stdio transport)
│   ├── thought.rs             # Thought processor (session, history, branches, recap)
│   ├── warnings.rs            # Warning engine (anti-quick-fix, budget, mode validation)
│   ├── analyzers/             # 5 cognitive analyzers (independent, merged into response)
│   │   ├── mod.rs             # Analyzer trait + runner
│   │   ├── depth.rs           # Topic overlap, contradiction detection, shallow analysis
│   │   ├── confidence.rs      # Independent confidence scoring, overconfidence alerts
│   │   ├── sycophancy.rs      # Premature agreement, no self-challenge, confirmation-only
│   │   ├── budget.rs          # Thought budgets, underthinking/overthinking
│   │   └── bias.rs            # Anchoring, confirmation, sunk cost, availability, overconfidence
│   ├── ml.rs                  # PerpetualBooster (predict per thought, train on session end)
│   └── config.rs              # Config loader (reads compiled TS configs or TOML fallback)
├── Cargo.toml
├── .claude/                   # Claude Code integration
│   ├── skills/                # Workflow skills (SKILL.md pattern)
│   ├── agents/                # Subagent definitions (YAML frontmatter .md)
│   └── resources/             # Style guides, templates
├── hooks/                     # Event-driven automations
│   ├── hooks.json             # Hook definitions
│   └── scripts/               # Hook implementations (TypeScript)
├── config/                    # Type-safe configuration (TypeScript)
│   ├── components.ts          # System component map (prevents hallucination)
│   ├── thresholds.ts          # Analyzer thresholds (confidence gap, budget ratios)
│   └── prompts.ts             # Tool descriptions (template literals)
├── rules/                     # Coding standards
│   ├── common/                # Universal (SOLID, KISS, DRY, security, testing)
│   └── rust/                  # Rust-specific (ownership, lifetimes, error handling)
├── _refs/                     # Reference material (cloned repos, deep dives)
├── CLAUDE.md
└── QUICKSTART_VALENCE.md
```

### Module Structure

| Rule | Requirement |
|------|-------------|
| **Single Responsibility** | Each module has one clear purpose. One file = one concern. |
| **Dependency Direction** | `analyzers/` → `thought.rs` → `main.rs`. ML is optional (never blocks core). |
| **Tests Alongside** | Rust: `#[cfg(test)] mod tests` in each file. TS: `*.test.ts` alongside source. |

---

## 4. Testing and Logging

### Test Environment

```bash
# Rust core
cargo test                    # Unit + integration tests
cargo test -- --nocapture     # With stdout

# TypeScript (hooks, config)
npx vitest run                # One-shot
npx vitest                    # Watch mode
```

### Logging

- Rust core: `tracing` crate with structured JSON output to stderr
- MCP responses: warnings array in JSON response (Claude sees these)
- ML layer: best-effort logging (never panic on log failure)
- Format: `{"level":"warn","module":"sycophancy","thought":3,"alert":"confirmation_only"}`

---

## 5. Schema & Protocol

### MCP Tool: `sequentialthinking` (enhanced)

**Input** (per thought):
```
thought: string              # Current reasoning step
thoughtNumber: int           # 1-indexed
totalThoughts: int           # Adjustable estimate
nextThoughtNeeded: bool      # Keep going or done?
thinkingMode?: enum          # architecture | performance | debugging | scaling | security
affectedComponents?: [str]   # From config/components.ts
confidence?: 0-100           # Self-reported
evidence?: [str]             # Citations (file paths, docs, measurements)
estimatedImpact?: {latency?, throughput?, risk?}
isRevision?: bool
revisesThought?: int
branchFromThought?: int
branchId?: str
```

**Output** (per thought):
```
thoughtNumber, totalThoughts, nextThoughtNeeded, branches[], thoughtHistoryLength
warnings: [str]              # Auto-generated alerts
analyzers: {                 # Merged analyzer output
  depth?: {...},
  confidence?: {reported, calculated, gap, alert?},
  sycophancy?: {pattern, severity, message},
  budget?: {used, total, category, alert?},
  bias?: {type, severity}
}
mlPrediction?: {             # PerpetualBooster output
  trajectoryScore: 0-1,
  driftDetected: bool,
  historicalPatterns?: [str]
}
recap?: str                  # Every 3rd thought
adr?: str                    # On completion (nextThoughtNeeded=false)
```

---

## 6. Deployment

### Build & Install

```bash
cargo build --release         # Produces single binary
# Binary goes to: target/release/feldspar

# Install as MCP server in Claude Code:
# ~/.claude/settings.json or project .mcp.json
{
  "mcpServers": {
    "feldspar": {
      "command": "./target/release/feldspar",
      "args": ["--config", "config/"]
    }
  }
}
```

### Distribution
- Single static binary (no runtime deps)
- Config files alongside (TS compiled to JSON at build time, or TOML fallback)
- `.claude/` directory copied to target project for skills/agents/resources

---

## 7. Security Considerations

| Area | Implementation |
|------|----------------|
| **MCP transport** | HTTP on localhost only (127.0.0.1:{port}, no external exposure). Origin header validation prevents DNS rebinding. |
| **Config loading** | Validated at startup. Invalid config = fail loud, don't fall back to defaults. |
| **ML model** | Best-effort. Corrupted model file = log + skip predictions, never crash. |
| **External models** | Valence_ext agents: read-only filesystem access. Write tools explicitly blocked. |
| **Secrets** | API keys via env vars only. Never in config files. `.env` in `.gitignore`. |

---

## 8. Performance & Optimization

| Strategy | Implementation |
|----------|----------------|
| **Single-process ML** | PerpetualBooster linked as Rust crate. No subprocess, no serialization overhead. Inference in microseconds. |
| **Analyzer independence** | All 5 analyzers run independently, can be parallelized (rayon). Merged into single response. |
| **Bounded history** | Thought history capped (configurable). Oldest evicted on overflow. Branches capped independently. |
| **Best-effort persistence** | ML training on session end is async. Never blocks thought processing. |
| **Lazy config** | Config loaded once at startup. Component maps and thresholds cached in memory. |

---

## 9. Lessons Learned

*Living section — add entries as patterns emerge or issues are resolved.*
