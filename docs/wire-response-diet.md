# Wire Response Diet

## Problem

Every thought, Claude reads feldspar's full WireResponse JSON. Most fields are echo-backs of values Claude already sent, empty arrays, or null Option fields that `skip_serializing_if` catches. On a clean thought with no issues, the response is ~300 tokens of mostly noise. Over a 10-thought trace, that's ~3000 tokens spent reading JSON that didn't change Claude's behavior.

Warnings work. The problem is everything around them.

## Current WireResponse

```rust
pub struct WireResponse {
    pub trace_id: String,              // echo-back
    pub thought_number: u32,           // echo-back
    pub total_thoughts: u32,           // echo-back
    pub next_thought_needed: bool,     // echo-back
    pub branches: Vec<String>,         // often empty
    pub thought_history_length: usize, // noise
    pub warnings: Vec<String>,         // often empty
    pub alerts: Vec<Alert>,            // the actual product
    pub confidence_reported: Option<f64>,  // echo-back
    pub confidence_calculated: Option<f64>,
    pub confidence_gap: Option<f64>,       // derivable
    pub bias_detected: Option<String>,
    pub sycophancy: Option<String>,
    pub depth_overlap: Option<f64>,
    pub budget_used: u32,
    pub budget_max: u32,
    pub budget_category: String,       // echo-back
    pub trajectory: Option<f64>,
    pub drift_detected: Option<bool>,
    pub recap: Option<String>,
    pub adr: Option<String>,
    pub trust_score: Option<f64>,
    pub trust_reason: Option<String>,
}
```

## Field-by-field analysis

### Strip always (echo-backs — Claude sent these values, already knows them)

| Field | Reason |
|-------|--------|
| `traceId` | Claude sent it in the request |
| `thoughtNumber` | Claude sent it |
| `totalThoughts` | Claude sent it |
| `nextThoughtNeeded` | Claude sent it |
| `confidenceReported` | Claude sent it |
| `budgetCategory` | Claude picked the mode, knows the category |
| `thoughtHistoryLength` | Claude can count its own thoughts |

### Strip when empty/zero (no information)

| Field | Reason |
|-------|--------|
| `warnings: []` | Empty array tells Claude nothing. Absence = no warnings. |
| `alerts: []` | Same — already uses `skip_serializing_if` but wasn't applied |

### Strip because derivable (Claude can compute these)

| Field | Reason |
|-------|--------|
| `confidenceGap` | `abs(reported - calculated)` — Claude can subtract |

### Keep only on first thought or when changed

| Field | Reason |
|-------|--------|
| `budgetMax` | Doesn't change between thoughts. Send on thought 1 only. |
| `branches` | Only send when a new branch is created |

### Always keep (new information Claude can't derive)

| Field | Reason |
|-------|--------|
| `alerts` | The core product — actionable warnings |
| `confidenceCalculated` | Feldspar's independent assessment |
| `biasDetected` | New signal |
| `sycophancy` | New signal |
| `budgetUsed` | Progress tracking |
| `trajectory` | ML prediction |
| `driftDetected` | ML signal |
| `recap` | Useful summary (every 3rd thought) |
| `adr` | Final output (completion only) |
| `trustScore` | Final output (completion only) |
| `trustReason` | Final output (completion only) |

### Maybe strip (questionable value)

| Field | Reason |
|-------|--------|
| `depthOverlap` | A raw float. Does Claude act on "overlap: 0.42"? Probably not. The SHALLOW_ANALYSIS alert already fires when it matters. The number itself is noise unless Claude is told what to do with it. |

## Result

### Clean thought (no issues)

Before (~300 tokens):
```json
{"traceId":"abc-123","thoughtNumber":3,"totalThoughts":5,
"nextThoughtNeeded":true,"thoughtHistoryLength":3,
"warnings":[],"budgetUsed":3,"budgetMax":5,
"budgetCategory":"standard","confidenceCalculated":18.75}
```

After (~10 tokens):
```json
{"budget":"3/5","confidence":19}
```

### Thought with alert (~50 tokens instead of ~400)

```json
{"budget":"3/5","confidence":19,
"alert":"OVERCONFIDENCE: reported 90% but evidence supports 19%"}
```

### First thought of trace (include one-time fields)

```json
{"budget":"1/5","budgetMax":5,"confidence":25}
```

### Completion thought (include final outputs)

```json
{"budget":"5/5","confidence":72,
"trustScore":7,"trustReason":"solid analysis, explored alternatives"}
```

## Implementation

1. Remove echo-back fields from `WireResponse` struct entirely
2. Add `skip_serializing_if` to `warnings` (empty vec) 
3. Compact `budgetUsed`/`budgetMax` into a single `budget: "3/5"` string
4. Round floats to integers — `18.75` → `19`. Claude doesn't need decimal precision.
5. Track `budgetMax` per session, only include when it differs from previous response
6. Flatten single alerts into a string instead of `Alert` struct with analyzer/kind/severity/message — Claude doesn't need the metadata, just the message.

## Token savings estimate

| Scenario | Before | After | Savings |
|----------|--------|-------|---------|
| Clean thought | ~300 tokens | ~10 tokens | 97% |
| Thought with 1 alert | ~400 tokens | ~50 tokens | 87% |
| 10-thought trace (2 alerts) | ~3200 tokens | ~200 tokens | 94% |
| Full session (5 traces) | ~16000 tokens | ~1000 tokens | 94% |
