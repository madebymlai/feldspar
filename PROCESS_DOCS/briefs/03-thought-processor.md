# Brief: Thought Processor — Recap, ADR, Eviction

**Problem**: The thought processor (issue #2) handles trace creation, lookup, and closing, but recaps, ADRs, and trace eviction are still no-ops. Without recaps, Claude loses context in long traces. Without ADRs, completed traces produce no decision record. Without eviction, closed traces leak memory.

**Requirements**:
- Recap every N thoughts (configurable via `config.feldspar.recap_every`, default 3)
- Recap via OpenRouter call to `gpt-oss-20b:nitro` with JSON output format (`{"recap": "..."}`)
- Recap scoped to current branch (filter by `branch_id`)
- Recap is inline (Claude waits for it, ~300ms)
- ADR skeleton on trace completion (`next_thought_needed == false`)
- ADR is a template filled from trace data — not an LLM call
- ADR includes: date, components, thinking modes used, decision (final thought), alternatives (from branches)
- Trace eviction after completion: `HashMap::remove()` + `Arc` for sharing with background tasks
- Background tasks (DB flush, trace review, ML train) receive `Arc<Trace>` — no clone of full trace data

**Constraints**:
- Recap uses same model as trace review (`gpt-oss-20b:nitro` via OpenRouter)
- Must use JSON output format (`{"recap": "..."}`) — this model returns `content: null` for freeform text
- API key from `config.trace_review.api_key_env` env var
- If OpenRouter call fails, skip recap (best-effort, never block response)
- ADR is template-based, not LLM-generated — instant, free
- Eviction pattern: `traces.remove()` → `Arc::new(trace)` → spawn background tasks with `Arc` clones
- DB/ML/trace review background tasks are still no-ops (issues #5-#7) — but the eviction + spawn pattern must be real

**Non-goals**:
- No analyzer pipeline (issue #4)
- No warning engine (issue #5)
- No real DB persistence (issue #6)
- No real ML inference (issue #7)
- No real trace review (issue #8) — but the recap OpenRouter call pattern is reusable for it

**Style**: Real recap LLM call, real eviction pattern, template ADR. The infrastructure for background tasks is wired even though the tasks themselves are no-ops.

**Key concepts**:
- **Recap**: LLM-generated 1-2 sentence summary of the last N thoughts, scoped to current branch. Returned in the WireResponse.
- **ADR**: Architecture Decision Record skeleton. Template filled from trace data on completion. Returned in the WireResponse.
- **Eviction**: Remove trace from HashMap after completion. Background tasks get `Arc<Trace>` ownership. Map stays clean.
- **JSON output trick**: `gpt-oss-20b:nitro` is a reasoning model. Asking for JSON forces content into the `content` field instead of `reasoning_details`.
