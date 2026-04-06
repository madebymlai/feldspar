# Brief: Cognitive Analyzers

**Problem**: Claude is systematically overconfident (ECE 0.45-0.81 on hard tasks), sycophantic with itself (CoT faithfulness ~25%, backwards-proof pattern confirmed by Anthropic interpretability), and prone to cognitive biases (17.8-57.3% susceptibility across bias types). The thought processor currently stores and recaps thoughts but never evaluates their quality. Issue #5 implements the 5 cognitive analyzers that inspect every thought and surface alerts when reasoning goes wrong.

**Requirements**:
- Observer/Evaluator two-phase pipeline as defined in `src/analyzers/mod.rs` stubs
- 3 Observers (depth, budget, bias) run in parallel via rayon, produce `Observation` data
- 2 Evaluators (confidence, sycophancy) run in parallel via rayon, consume observations and produce `Option<Alert>`
- Entry point: `run_pipeline(input: &ThoughtInput, records: &[ThoughtRecord], config: &Config) -> (Vec<Alert>, Observations)`
- `catch_unwind` per analyzer — a panicking analyzer produces a warning in the response, never crashes the pipeline
- All analysis is pure heuristics: `strsim` similarity, keyword matching, counting, domain antonym pairs, simple math. Zero LLM calls.
- Wire into `process_thought()` in `src/thought.rs` — alerts populate `WireResponse.alerts`, observations populate relevant `WireResponse` fields

**Depth observer** (`src/analyzers/depth.rs`):
- `overlap: f64` — strsim similarity between current thought text and previous thought on same branch
- Threshold bands: >0.7 = rephrasing, 0.3-0.7 = building on (normal), <0.3 = topic switch
- Below 0.3 overlap fires `PREMATURE_TOPIC_SWITCH` alert
- Contradiction detection — three heuristic layers (no LLM):
  1. Negation flip: high strsim pair (>0.7) where one has negation word the other doesn't ("not", "never", "no", "cannot", "shouldn't", "won't")
  2. Domain antonym pairs: same grammatical subject, antonym predicate. Domain list: sync/async, blocking/non-blocking, stateful/stateless, mutable/immutable, eager/lazy, push/pull, static/dynamic, compile-time/runtime, sequential/parallel, cached/uncached, persistent/ephemeral, increase/decrease, add/remove, enable/disable, allow/deny, accept/reject, create/destroy, valid/invalid, safe/unsafe
  3. Quantifier conflict: "all" vs "none", "always" vs "never", "every" vs "no", "must" vs "may not", "required" vs "optional"
- Fires `UNRESOLVED_CONTRADICTION` when any layer detects contradiction between two thoughts not marked as revision of each other
- Shallow analysis: if >50% of thoughts on branch have overlap >0.7 with their predecessor AND introduce no new named entities/numbers, fires `SHALLOW_ANALYSIS`
- Source: Stanford ACL 2008 "Finding Contradictions in Text" — antonymy alignment is "a very good cue for contradiction"

**Budget observer** (`src/analyzers/budget.rs`):
- **Observation only — no alerts** (warning engine in `src/warnings.rs` owns budget alerts per issue #4 AR decision)
- Returns `Observation::Budget { used, max, category }` for evaluators to consume
- Budget tiers from config: minimal [2,3], standard [3,5], deep [5,8]
- Research backing: inverted U-curve for CoT length (arXiv:2502.07266) — optimal 4-14 steps depending on model, both underthinking and overthinking degrade quality

**Bias observer** (`src/analyzers/bias.rs`):
- Checks 5 cognitive biases (first match wins):
  - `anchoring`: strsim >0.75 between thought 1 conclusion and current conclusion (strip stop words), AND no branches explored. Strong models show MORE anchoring (arXiv:2412.06593).
  - `confirmation`: all evidence keywords ("confirms", "supports", "as expected", "validates", "consistent with") present, zero counter-argument keywords ("however", "but", "alternatively", "weakness", "risk", "downside", "what if", "consider that"). Source: arXiv:2603.18740 (framing a change as bug-free reduced vulnerability detection 16-93%).
  - `sunk_cost`: two-keyword requirement to cut false positives. SET A (anchor): "already built/implemented/written/invested", "we've spent", "can't throw away", "too much work", "not worth rewriting". SET B (continuation, same or adjacent thought): "so we should keep", "better to keep", "might as well", "instead of starting over", "push through". Trigger: SET_A AND SET_B match, no question word present.
  - `availability`: same named entity/tech noun appears in >40% of thoughts on branch AND surrounding sentences add no new predicates/numbers/evidence each time. Better than the original 75% keyword frequency (no research basis for that number).
  - `overconfidence` (timing): confidence >80 AND thought_number/total_thoughts <0.5. Complements confidence evaluator — catches timing, not evidence.
- Returns `Observation::Bias { detected: Option<String> }` — the bias name or None

**Confidence evaluator** (`src/analyzers/confidence.rs`):
- Independent scoring rubric (max 80 raw, normalized to 0-100):
  - Evidence cited: 0-30 pts (10 per citation, max 3)
  - Alternatives explored: 0-25 pts (any branch in trace = 25)
  - Contradictions detected: -20 pts penalty (from observations.depth.contradictions)
  - Substance score: 0-15 pts — 2x2 matrix based on hedging x evidence:
    - No hedging + evidence present = 15 pts (confident and grounded)
    - Hedging + evidence present = 10 pts (calibrated uncertainty)
    - No hedging + no evidence = 5 pts (overconfident penalty — separate from the gap alert)
    - Hedging + no evidence = 0 pts (uncertain and ungrounded)
    - Harmful words (validated, arXiv:2508.15842): "complexity", "guess", "stuck", "hard", "probably", "possibly", "likely", "perhaps", "maybe", "I think", "I believe"
  - Bias avoidance: 0-10 pts (observations.bias_detected is None = 10)
- `OVERCONFIDENCE` alert when |reported - calculated| > config.thresholds.confidence_gap (25 points)
- Research: Claude ECE 0.45-0.81 on hard tasks (arXiv:2502.11028). Harmful word detection (MCC 0.354) outperforms self-reported confidence (MCC 0.065).

**Sycophancy evaluator** (`src/analyzers/sycophancy.rs`):
- Three patterns (first match wins):
  - `PREMATURE_AGREEMENT` (Severity::Medium): thoughts 1-2 both agree with premise, no challenge keywords ("however", "alternatively", "on the other hand", "risk", "downside", "but", "problem", "issue"). Checked at thought_number == 2.
  - `NO_SELF_CHALLENGE` (Severity::Medium): 3+ consecutive thoughts on same branch with no branching and no revision. Sliding window over last 3 thoughts.
  - `CONFIRMATION_ONLY` (Severity::High): next_thought_needed=false AND strsim >0.7 between thought 1 and final thought AND zero counter-argument keywords in ANY intermediate thought AND zero revisions AND zero branches. Most dangerous pattern — backwards-proof / motivated reasoning.
- Research: Anthropic confirmed Claude "works backwards" to fabricate proofs (anthropic.com/research/tracing-thoughts-language-model). CoT faithfulness ~25% — models conceal sycophancy influence in 75-99% of cases (arXiv:2505.05410).
- Counter-argument keywords (unvalidated heuristic, no paper source — synthesized from argumentation literature): "however", "but", "contrary", "alternatively", "problem with this", "weakness", "downside", "risk", "what if", "on the other hand", "counter", "drawback"
- Reads `observations.depth_overlap` for thought 1 vs final comparison

**Constraints**:
- Pure heuristics only — no LLM calls in the analyzer pipeline
- Rayon for parallelism within each phase (observers parallel, then evaluators parallel)
- `catch_unwind` per analyzer with warning surfaced in response
- Must not block the thought processor — analyzers are synchronous, called within the write-lock-free Phase 2
- All thresholds from `config/feldspar.toml` — no hardcoded magic numbers (except scoring rubric weights which are structural)
- `strsim` already in `Cargo.toml`, `rayon` needs to be added
- Budget observer is observation-only (warning engine owns budget alerts)
- `warnings` field (Vec<String>) stays separate from `alerts` field (Vec<Alert>)

**Non-goals**:
- LLM-based analysis (trace review, issue #7)
- ML predictions (issue #8)
- Warning engine / mode validation (issue #4, already implemented)
- Modifying the MCP layer or config loader

**Style**: Fast and deterministic. Microsecond-scale per thought (except rayon thread pool startup on first call). Alerts are terse and actionable — Claude reads them, not humans.

**Key concepts**:
- **Observer**: Extracts signals from the thought/trace. No cross-dependencies between observers.
- **Evaluator**: Consumes observer output to make higher-order judgments. Reads the merged `Observations` struct.
- **Observation**: Data produced by an observer (overlap scores, budget counts, bias detection). Passed to evaluators.
- **Alert**: Actionable finding with `analyzer`, `kind`, `severity`, `message`. Surfaced in `WireResponse.alerts` for Claude to see.
- **Pipeline**: Observers run first (parallel via rayon), observations merged, evaluators run second (parallel via rayon). Two-phase.

**Research citations**:
- Stanford ACL 2008: "Finding Contradictions in Text" — antonymy alignment for contradiction detection
- arXiv:2502.11028: Claude overconfidence ECE 0.45-0.81 on hard tasks
- arXiv:2508.15842: Harmful word detection (MCC 0.354) outperforms self-reported confidence (MCC 0.065)
- arXiv:2502.07266: Inverted U-curve for CoT length, optimal 4-14 steps
- arXiv:2412.06593: Strong models show more anchoring bias than weak ones
- arXiv:2603.18740: Confirmation bias in code review — framing reduces detection 16-93%
- arXiv:2505.05410: CoT faithfulness ~25%, sycophancy concealed in 75-99% of cases
- Anthropic: "Tracing the Thoughts of a Large Language Model" — backwards proof / motivated reasoning
- arXiv:2412.03605: CBEval — LLM bias susceptibility 17.8-57.3%
