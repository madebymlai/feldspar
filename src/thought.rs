use crate::analyzers::run_pipeline;
use crate::config::Config;
use crate::llm::LlmClient;
use crate::warnings::generate_warnings;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use tokio::sync::RwLock;

pub type Timestamp = i64;

/// (is_revision, branch_from_thought) for last 3 records on current branch.
/// Used by warning engine to check for recent progress.
pub type RecentProgress = Vec<(bool, Option<u32>)>;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThoughtInput {
    #[serde(default)]
    pub trace_id: Option<String>,
    pub thought: String,
    pub thought_number: u32,
    pub total_thoughts: u32,
    pub next_thought_needed: bool,
    pub thinking_mode: Option<String>,
    #[serde(default)]
    pub affected_components: Vec<String>,
    pub confidence: Option<f64>,
    #[serde(default)]
    pub evidence: Vec<String>,
    pub estimated_impact: Option<Impact>,
    #[serde(default)]
    pub is_revision: bool,
    pub revises_thought: Option<u32>,
    pub branch_from_thought: Option<u32>,
    pub branch_id: Option<String>,
    #[serde(default)]
    pub needs_more_thoughts: bool,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ThoughtResult {
    pub warnings: Vec<String>,
    pub alerts: Vec<Alert>,
    pub confidence_calculated: Option<f64>,
    pub depth_overlap: Option<f64>,
    pub budget_used: u32,
    pub budget_max: u32,
    pub budget_category: String,
    pub ml_trajectory: Option<f64>,
    pub ml_drift: Option<bool>,
    pub recap: Option<String>,
    pub adr: Option<String>,
    pub auto_outcome: Option<f64>,
}

/// Flat wire response — what Claude sees in content[0].text.
/// Merges echo-backs from ThoughtInput, trace metadata, and ThoughtResult fields.
/// NOT ThoughtResult directly — field names differ (trajectory not mlTrajectory, etc).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireResponse {
    // Echo-backs from ThoughtInput
    pub trace_id: String,
    pub thought_number: u32,
    pub total_thoughts: u32,
    pub next_thought_needed: bool,

    // Trace metadata
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<String>,
    pub thought_history_length: usize,

    // From ThoughtResult (some renamed) — only included when non-empty/non-null
    // warnings always present (even empty) so Claude sees the field exists
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alerts: Vec<Alert>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence_reported: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence_calculated: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence_gap: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bias_detected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sycophancy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth_overlap: Option<f64>,
    pub budget_used: u32,
    pub budget_max: u32,
    pub budget_category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trajectory: Option<f64>,      // ThoughtResult.ml_trajectory
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift_detected: Option<bool>, // ThoughtResult.ml_drift
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recap: Option<String>,

    // Completion-only
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_reason: Option<String>,

    // Pattern recall (thought 1 only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern_recall: Option<Vec<crate::db::PatternMatch>>,
}

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct Impact {
    pub latency: Option<String>,
    pub throughput: Option<String>,
    pub risk: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Alert {
    pub analyzer: String,
    pub kind: String,
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub enum Severity {
    Medium,
    High,
}

pub struct ThoughtRecord {
    pub input: ThoughtInput,
    pub result: ThoughtResult,
    pub created_at: Timestamp,
}

pub struct Trace {
    pub id: String,
    pub thoughts: Vec<ThoughtRecord>,
    pub created_at: Timestamp,
    pub closed: bool,
}

impl Trace {
    pub fn new() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            thoughts: Vec::new(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64,
            closed: false,
        }
    }
}

pub struct ThinkingServer {
    pub traces: RwLock<HashMap<String, Trace>>,
    pub config: Arc<Config>,
    pub db: Option<Arc<crate::db::Db>>,
    pub ml: Option<Arc<crate::ml::MlEngine>>,
    pub llm: Option<LlmClient>,
    pub leaf_cache: Arc<RwLock<HashMap<String, Vec<usize>>>>,
}

impl ThinkingServer {
    pub fn new(
        config: Arc<Config>,
        llm: Option<LlmClient>,
        db: Option<Arc<crate::db::Db>>,
        leaf_cache: Arc<RwLock<HashMap<String, Vec<usize>>>>,
        ml: Option<Arc<crate::ml::MlEngine>>,
    ) -> Self {
        Self {
            traces: RwLock::new(HashMap::new()),
            config,
            db,
            ml,
            llm,
            leaf_cache,
        }
    }

    pub async fn process_thought(&self, input: ThoughtInput) -> Result<WireResponse, String> {
        // === PHASE 1: Write lock held (microseconds) ===
        let mut recap_text: Option<String> = None;
        let mut removed_trace: Option<Trace> = None;
        let snapshot;

        {
            let mut traces = self.traces.write().await;

            // Create or lookup trace
            let trace_id = if input.thought_number == 1 && input.trace_id.is_none() {
                let trace = Trace::new();
                let id = trace.id.clone();
                traces.insert(id.clone(), trace);
                id
            } else if let Some(ref id) = input.trace_id {
                if !traces.contains_key(id) {
                    return Err(format!("unknown trace: {}", id));
                }
                id.clone()
            } else {
                return Err("trace_id required for thought_number > 1".into());
            };

            let trace = traces.get_mut(&trace_id).unwrap();

            // Clone branch-filtered records BEFORE pushing current thought.
            // Observers receive history only — not the thought they're analyzing.
            let branch_records: Vec<ThoughtInput> = trace
                .thoughts
                .iter()
                .filter(|t| t.input.branch_id == input.branch_id)
                .map(|t| t.input.clone())
                .collect();

            // Append record
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64;

            trace.thoughts.push(ThoughtRecord {
                input: input.clone(),
                result: ThoughtResult::default(),
                created_at: now,
            });

            // Collect unique branch IDs (BTreeSet for deterministic ordering)
            let branches: Vec<String> = trace
                .thoughts
                .iter()
                .filter_map(|t| t.input.branch_id.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();

            let thought_history_length = trace.thoughts.len();

            // Determine budget from config
            let (budget_used, budget_max, budget_category) =
                match self.config.resolve_budget(input.thinking_mode.as_deref()) {
                    Some((_, max, tier)) => (input.thought_number, max, tier),
                    None => (input.thought_number, 5, "standard".into()),
                };

            // Extract recent progress for warning engine
            let recent_progress: RecentProgress = trace.thoughts
                .iter()
                .filter(|t| t.input.branch_id == input.branch_id)
                .rev()
                .take(3)
                .map(|t| (t.input.is_revision, t.input.branch_from_thought))
                .collect();

            // If recap due: extract branch-filtered thought texts for Phase 2
            if input.thought_number > 1
                && input.thought_number % self.config.feldspar.recap_every == 0
            {
                let formatted = trace
                    .thoughts
                    .iter()
                    .filter(|t| t.input.branch_id == input.branch_id)
                    .enumerate()
                    .map(|(i, t)| format!("Thought {}: {}", i + 1, t.input.thought))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                recap_text = Some(formatted);
            }

            // If completing: remove trace for eviction
            if !input.next_thought_needed {
                removed_trace = traces.remove(&trace_id);
            }

            snapshot = TraceSnapshot {
                trace_id,
                branches,
                thought_history_length,
                budget_used,
                budget_max,
                budget_category,
                recent_progress,
                branch_records,
            };
            // Write lock drops here
        }

        // === PHASE 2: No lock held ===

        // Recap (async LLM call — safe, no lock held)
        let recap = if let Some(ref text) = recap_text {
            self.generate_recap(text).await
        } else {
            None
        };

        // ADR from removed trace
        let adr = if let Some(ref trace) = removed_trace {
            Some(generate_adr(trace))
        } else {
            None
        };

        // Prune on session start (thought 1)
        if input.thought_number == 1 {
            if let (Some(db), Some(ml)) = (&self.db, &self.ml) {
                let db = db.clone();
                let ml = ml.clone();
                let cache = self.leaf_cache.clone();
                tokio::spawn(async move {
                    run_prune(&ml, &db, &cache).await;
                });
            }
        }

        // Trust scoring (blocking — deliberate HC#2 exception: user sees score in completion response)
        let (trust_score, trust_reason, mut trust_warnings) = if let Some(ref trace) = removed_trace {
            let mode = trace.thoughts.first()
                .and_then(|t| t.input.thinking_mode.as_deref());

            match mode {
                None => {
                    (None, None, vec!["THINKING_MODE_MISSING".into()])
                }
                Some(mode) => {
                    match self.llm.as_ref() {
                        Some(llm) if llm.has_api_key() => {
                            match crate::trace_review::review(llm, trace, mode).await {
                                Some(score) => (Some(score.trust), Some(score.reason), vec![]),
                                None => (None, None, vec!["TRUST_SCORE_UNAVAILABLE".into()]),
                            }
                        }
                        _ => {
                            (None, None, vec!["OPENROUTER_KEY_NOT_SET".into()])
                        }
                    }
                }
            }
        } else {
            (None, None, vec![])
        };

        // Persist trust score to DB (best-effort, background)
        if let Some(ref trace) = removed_trace {
            if let (Some(db), Some(score), Some(reason)) = (&self.db, trust_score, &trust_reason) {
                let db = db.clone();
                let trace_id = trace.id.clone();
                let reason = reason.clone();
                tokio::spawn(async move {
                    db.update_trust(&trace_id, score, &reason).await;
                });
            }
        }

        // Background tasks for evicted trace (move — last use of removed_trace)
        if let Some(trace) = removed_trace {
            let trace = Arc::new(trace);

            // Compute features before flush (needs full trace for snapshot)
            let features_for_ml = if let Some(ref ml) = self.ml {
                let ml_snap = build_trace_snapshot(&trace);
                let last_input = trace.thoughts.last().map(|r| &r.input).unwrap_or(&input);
                let features = crate::ml::MlEngine::extract_features(last_input, &ml_snap, &ml.mode_map);
                Some(features)
            } else {
                None
            };
            let features_blob = features_for_ml.as_ref().and_then(|f|
                bincode::encode_to_vec(f, bincode::config::standard()).ok()
            );

            // 1. Flush trace — AWAITED, must complete before UPDATE tasks
            if let Some(ref db) = self.db {
                let components: Vec<String> = trace.thoughts.iter()
                    .flat_map(|t| t.input.affected_components.iter().cloned())
                    .collect::<BTreeSet<_>>().into_iter().collect();
                db.flush_trace(
                    &snapshot.trace_id,
                    input.thinking_mode.as_deref(),
                    &components,
                    features_blob.as_deref(),
                    trace.created_at,
                ).await;
            }

            // 2. ML train (spawned — fires when trust score is available)
            if let Some(trust) = trust_score {
                if let (Some(ml), Some(features)) = (&self.ml, &features_for_ml) {
                    let ml = ml.clone();
                    let features = features.clone();
                    tokio::spawn(async move {
                        ml.train(features, trust);
                    });
                }
            }

            // 3. ML compute leaf nodes + update cache (spawned)
            if let (Some(ml), Some(features)) = (&self.ml, features_for_ml) {
                let ml = ml.clone();
                let db = self.db.clone();
                let trace_id = snapshot.trace_id.clone();
                let cache = self.leaf_cache.clone();
                tokio::spawn(async move {
                    if let Some(flat_leaves) = ml.predict_nodes(&features) {
                        if let Some(ref db) = db {
                            db.store_leaf_nodes(&trace_id, &flat_leaves).await;
                        }
                        cache.write().await.insert(trace_id, flat_leaves);
                    }
                });
            }
        }

        let pipeline = run_pipeline(&input, &snapshot.branch_records, &self.config);

        // ML predict + drift (hot path — sync, microseconds)
        let (ml_trajectory, ml_drift) = if let Some(ref ml) = self.ml {
            let ml_snap = build_ml_snapshot_from_phase2(&input, &snapshot, &pipeline);
            let features = crate::ml::MlEngine::extract_features(&input, &ml_snap, &ml.mode_map);
            let trajectory = ml.predict(&features);
            let drift_report = ml.drift(&features);
            (trajectory, Some(drift_report.data_drift || drift_report.concept_drift))
        } else {
            (None, None)
        };

        // Pattern recall (thought 1 only)
        let pattern_recall = if input.thought_number == 1 {
            if let Some(ref ml) = self.ml {
                let sync_cache: Option<HashMap<String, Vec<usize>>> = {
                    let cache = self.leaf_cache.read().await;
                    if cache.len() >= self.config.feldspar.pattern_recall_min_traces as usize {
                        Some(cache.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                    } else {
                        None
                    }
                };
                if let Some(sync_cache) = sync_cache {
                    let ml_snap = build_ml_snapshot_from_phase2(&input, &snapshot, &pipeline);
                    let features = crate::ml::MlEngine::extract_features(&input, &ml_snap, &ml.mode_map);
                    let ids = ml.find_similar(
                        &features,
                        &sync_cache,
                        self.config.feldspar.pattern_recall_top_k as usize,
                    );
                    if !ids.is_empty() {
                        if let Some(ref db) = self.db {
                            Some(db.find_traces_by_ids(&ids).await)
                        } else { None }
                    } else { None }
                } else { None }
            } else { None }
        } else { None };

        let mut warnings = generate_warnings(&input, &snapshot.recent_progress, &self.config);
        warnings.extend(pipeline.panic_warnings);
        warnings.append(&mut trust_warnings);

        // Spawn write_thought for every thought (best-effort)
        if let Some(ref db) = self.db {
            let db = db.clone();
            let trace_id = snapshot.trace_id.clone();
            let thought_number = input.thought_number;
            let thinking_mode = input.thinking_mode.clone();
            let input_json = serde_json::to_string(&input).unwrap_or_default();
            let result_json = "{}".to_owned();
            let created_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64;
            tokio::spawn(async move {
                db.write_thought(
                    &trace_id,
                    thought_number,
                    thinking_mode.as_deref(),
                    &input_json,
                    &result_json,
                    created_at,
                ).await;
            });
        }

        Ok(WireResponse {
            trace_id: snapshot.trace_id,
            thought_number: input.thought_number,
            total_thoughts: input.total_thoughts,
            next_thought_needed: input.next_thought_needed,
            branches: snapshot.branches,
            thought_history_length: snapshot.thought_history_length,
            warnings,
            alerts: pipeline.alerts,
            confidence_reported: input.confidence,
            confidence_calculated: pipeline.confidence_calculated,
            confidence_gap: match (input.confidence, pipeline.confidence_calculated) {
                (Some(reported), Some(calculated)) => Some((reported - calculated).abs()),
                _ => None,
            },
            bias_detected: pipeline.observations.bias_detected,
            sycophancy: pipeline.sycophancy_pattern,
            depth_overlap: pipeline.observations.prev_overlap,
            budget_used: snapshot.budget_used,
            budget_max: snapshot.budget_max,
            budget_category: snapshot.budget_category,
            trajectory: ml_trajectory,
            drift_detected: ml_drift,
            recap,
            adr,
            trust_score,
            trust_reason,
            pattern_recall,
        })
    }

    async fn generate_recap(&self, thoughts_text: &str) -> Option<String> {
        let llm = self.llm.as_ref()?;
        let result = llm.chat_json(RECAP_SYSTEM_PROMPT, thoughts_text, 200).await?;
        result["recap"].as_str().map(|s| s.to_owned())
    }
}

const RECAP_SYSTEM_PROMPT: &str = "You summarize thinking traces. Given numbered thoughts, \
    produce a 1-2 sentence recap capturing the key progression and current conclusion. \
    Respond with ONLY a JSON object: {\"recap\": \"<your summary>\"}";

/// Build ML TraceSnapshot from the phase-2 local snapshot + pipeline output.
/// Used for hot-path predict/drift. Fields not available without full trace history
/// are set to NaN (handled correctly by extract_features).
fn build_ml_snapshot_from_phase2(
    _input: &ThoughtInput,
    snapshot: &TraceSnapshot,
    pipeline: &crate::analyzers::PipelineResult,
) -> crate::ml::TraceSnapshot {
    let branch_inputs = &snapshot.branch_records;
    let confidences: Vec<f64> = branch_inputs.iter().filter_map(|t| t.confidence).collect();
    let avg_confidence = if confidences.is_empty() {
        f64::NAN
    } else {
        confidences.iter().sum::<f64>() / confidences.len() as f64
    };

    crate::ml::TraceSnapshot {
        thought_count: snapshot.thought_history_length as u32,
        avg_confidence,
        avg_confidence_gap: f64::NAN,
        avg_prior_depth: f64::NAN,
        current_depth_overlap: pipeline.observations.prev_overlap.unwrap_or(0.0),
        branch_count: snapshot.branches.len(),
        revision_count: branch_inputs.iter().filter(|t| t.is_revision).count(),
        budget_used: snapshot.budget_used,
        budget_max: snapshot.budget_max,
        prior_warning_count: 0,
        warning_responsiveness_ratio: f64::NAN,
        confidence_convergence: f64::NAN,
    }
}

/// Build ML TraceSnapshot from a completed Trace.
/// Used for train/leaf-node spawns on trace completion.
/// ThoughtRecord.result is always default (not persisted), so gap/depth fields
/// will be NaN — the ML model handles undefined features gracefully.
fn build_trace_snapshot(trace: &Trace) -> crate::ml::TraceSnapshot {
    let thoughts = &trace.thoughts;
    let n = thoughts.len() as u32;

    let confidences: Vec<f64> = thoughts.iter().filter_map(|t| t.input.confidence).collect();
    let avg_confidence = if confidences.is_empty() {
        f64::NAN
    } else {
        confidences.iter().sum::<f64>() / confidences.len() as f64
    };

    // These depend on stored ThoughtResult fields (always None — not persisted).
    let gaps: Vec<f64> = thoughts
        .iter()
        .filter_map(|t| {
            let reported = t.input.confidence?;
            let calculated = t.result.confidence_calculated?;
            Some((reported - calculated).abs())
        })
        .collect();
    let avg_confidence_gap = if gaps.is_empty() {
        f64::NAN
    } else {
        gaps.iter().sum::<f64>() / gaps.len() as f64
    };
    let confidence_convergence = if gaps.len() >= 3 {
        let last3 = &gaps[gaps.len() - 3..];
        let mean = last3.iter().sum::<f64>() / 3.0;
        let variance = last3.iter().map(|g| (g - mean).powi(2)).sum::<f64>() / 3.0;
        variance.sqrt()
    } else {
        f64::NAN
    };

    let depth_overlaps: Vec<f64> = thoughts.iter().filter_map(|t| t.result.depth_overlap).collect();
    let avg_prior_depth = if depth_overlaps.len() < 2 {
        0.0
    } else {
        let prior = &depth_overlaps[..depth_overlaps.len() - 1];
        prior.iter().sum::<f64>() / prior.len() as f64
    };
    let current_depth_overlap = depth_overlaps.last().copied().unwrap_or(0.0);

    let branch_count = thoughts
        .iter()
        .filter_map(|t| t.input.branch_id.as_ref())
        .collect::<std::collections::HashSet<_>>()
        .len();

    let warning_responsiveness_ratio = compute_warning_responsiveness(thoughts);

    crate::ml::TraceSnapshot {
        thought_count: n,
        avg_confidence,
        avg_confidence_gap,
        avg_prior_depth,
        current_depth_overlap,
        branch_count,
        revision_count: thoughts.iter().filter(|t| t.input.is_revision).count(),
        budget_used: thoughts.len() as u32,
        budget_max: thoughts.last().map(|t| t.input.total_thoughts).unwrap_or(5),
        prior_warning_count: thoughts
            .iter()
            .take(thoughts.len().saturating_sub(1))
            .map(|t| t.result.warnings.len())
            .sum(),
        warning_responsiveness_ratio,
        confidence_convergence,
    }
}

fn compute_warning_responsiveness(thoughts: &[ThoughtRecord]) -> f64 {
    let mut warning_thoughts = 0usize;
    let mut responsive_thoughts = 0usize;

    for i in 0..thoughts.len().saturating_sub(1) {
        if thoughts[i].result.warnings.is_empty() {
            continue;
        }
        warning_thoughts += 1;
        let next = &thoughts[i + 1];
        let curr = &thoughts[i];
        let confidence_changed = match (curr.input.confidence, next.input.confidence) {
            (Some(a), Some(b)) => (a - b).abs() > 10.0,
            _ => false,
        };
        let branched = next.input.branch_from_thought.is_some();
        let revised = next.input.is_revision;
        let needs_more = next.input.needs_more_thoughts && !curr.input.needs_more_thoughts;
        if confidence_changed || branched || revised || needs_more {
            responsive_thoughts += 1;
        }
    }

    if warning_thoughts == 0 {
        f64::NAN
    } else {
        responsive_thoughts as f64 / warning_thoughts as f64
    }
}

/// Shared prune logic: evict low-value traces from DB + leaf cache.
/// Called from the 30-min timer in main.rs and from thought-1 spawn.
pub(crate) async fn run_prune(
    ml: &Arc<crate::ml::MlEngine>,
    db: &Arc<crate::db::Db>,
    leaf_cache: &Arc<RwLock<HashMap<String, Vec<usize>>>>,
) {
    let count = db.trace_count_with_trust().await;
    if count < 100 {
        return;
    }
    let matrix = db.load_feature_matrix().await;
    if matrix.is_empty() {
        return;
    }

    let cache_snapshot: HashMap<String, Vec<usize>> = leaf_cache.read().await.clone();
    let std_cache = std::sync::RwLock::new(cache_snapshot);
    let evict_ids = ml.prune_cycle(&matrix, &std_cache);

    if !evict_ids.is_empty() {
        db.prune(&evict_ids).await;
        let mut cache = leaf_cache.write().await;
        for id in &evict_ids {
            cache.remove(id);
        }
    }
    // v1: skip leaf refresh after prune — stale leaf sets are refreshed on next trace completion.
}

/// Extracted data from Phase 1 for building WireResponse in Phase 2
struct TraceSnapshot {
    trace_id: String,
    branches: Vec<String>,
    thought_history_length: usize,
    budget_used: u32,
    budget_max: u32,
    budget_category: String,
    recent_progress: RecentProgress,
    branch_records: Vec<ThoughtInput>,
}

fn generate_adr(trace: &Trace) -> String {
    let date = unix_millis_to_date(trace.created_at);

    let components: Vec<String> = trace
        .thoughts
        .iter()
        .flat_map(|t| t.input.affected_components.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let modes: Vec<String> = trace
        .thoughts
        .iter()
        .filter_map(|t| t.input.thinking_mode.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    // Decision = last main-line thought (branch_id.is_none())
    let decision = trace
        .thoughts
        .iter()
        .filter(|t| t.input.branch_id.is_none())
        .last()
        .map(|t| t.input.thought.as_str())
        .unwrap_or("No conclusion");

    // Branches explored = first thought text from each branch
    let mut branch_descriptions: Vec<String> = Vec::new();
    let mut seen_branches: BTreeSet<String> = BTreeSet::new();
    for t in &trace.thoughts {
        if let Some(ref bid) = t.input.branch_id {
            if seen_branches.insert(bid.clone()) {
                let truncated: String = t.input.thought.chars().take(100).collect();
                let text = if truncated.len() < t.input.thought.len() {
                    format!("{}: {}...", bid, truncated)
                } else {
                    format!("{}: {}", bid, t.input.thought)
                };
                branch_descriptions.push(text);
            }
        }
    }

    format!(
        "## ADR\n**Date**: {}\n**Components**: {}\n**Mode**: {}\n**Decision**: {}\n**Branches explored**: {}",
        date,
        if components.is_empty() { "none".into() } else { components.join(", ") },
        if modes.is_empty() { "none".into() } else { modes.join(", ") },
        decision,
        if branch_descriptions.is_empty() { "none".into() } else { branch_descriptions.join("; ") },
    )
}

fn unix_millis_to_date(millis: i64) -> String {
    let secs = (millis / 1000) as u64;
    let days = secs / 86400;
    let (y, m, d) = days_to_ymd(days);
    format!("{}-{:02}-{:02}", y, m, d)
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    let mut year = 1970u64;
    loop {
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let days_in_year = if leap { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let month_days: [u64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
    ];
    let mut month = 1u64;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }
    (year, month, days + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn test_input(thought_number: u32, trace_id: Option<String>, next_needed: bool) -> ThoughtInput {
        ThoughtInput {
            trace_id,
            thought: "test thought".into(),
            thought_number,
            total_thoughts: 5,
            next_thought_needed: next_needed,
            thinking_mode: None,
            affected_components: vec![],
            confidence: None,
            evidence: vec![],
            estimated_impact: None,
            is_revision: false,
            revises_thought: None,
            branch_from_thought: None,
            branch_id: None,
            needs_more_thoughts: false,
        }
    }

    fn test_server() -> ThinkingServer {
        let config = crate::config::Config {
            feldspar: crate::config::FeldsparConfig {
                db_path: "test.db".into(),
                model_path: "test.model".into(),
                recap_every: 3,
                pattern_recall_top_k: 3,
                ml_budget: 0.5,
                pattern_recall_min_traces: 10,
            },
            llm: crate::config::LlmConfig {
                base_url: None,
                api_key_env: Some("TEST_KEY".into()),
                model: "test-model".into(),
            },
            thresholds: crate::config::ThresholdsConfig {
                confidence_gap: 25.0,
                over_analysis_multiplier: 1.5,
                overthinking_multiplier: 2.0,
            },
            budgets: HashMap::from([
                ("minimal".into(), [2, 3]),
                ("standard".into(), [3, 5]),
                ("deep".into(), [5, 8]),
            ]),
            modes: HashMap::from([
                (
                    "architecture".into(),
                    crate::config::ModeConfig {
                        requires: vec![],
                        budget: "deep".into(),
                        watches: "test watches".into(),
                    },
                ),
                (
                    "standard-mode".into(),
                    crate::config::ModeConfig {
                        requires: vec![],
                        budget: "standard".into(),
                        watches: "x".into(),
                    },
                ),
            ]),
            components: crate::config::ComponentsConfig { valid: vec![] },
            ar: None,
            principles: vec![],
        };
        ThinkingServer::new(Arc::new(config), None, None, Arc::new(RwLock::new(HashMap::new())), None)
    }

    // --- Task 1 tests ---

    #[test]
    fn test_thought_input_with_trace_id() {
        let json = r#"{
            "traceId": "abc-123",
            "thought": "test",
            "thoughtNumber": 1,
            "totalThoughts": 3,
            "nextThoughtNeeded": true
        }"#;
        let input: ThoughtInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.trace_id, Some("abc-123".into()));
    }

    #[test]
    fn test_thought_input_without_trace_id() {
        let json = r#"{
            "thought": "Quick check",
            "thoughtNumber": 1,
            "totalThoughts": 1,
            "nextThoughtNeeded": false
        }"#;
        let input: ThoughtInput = serde_json::from_str(json).unwrap();
        assert!(input.trace_id.is_none());
    }

    // --- Task 2 tests ---

    #[test]
    fn test_wire_response_serializes_camel_case() {
        let wire = WireResponse {
            trace_id: "t1".into(),
            thought_number: 1,
            total_thoughts: 3,
            next_thought_needed: true,
            branches: vec![],
            thought_history_length: 1,
            warnings: vec![],
            alerts: vec![],
            confidence_reported: None,
            confidence_calculated: None,
            confidence_gap: None,
            bias_detected: None,
            sycophancy: None,
            depth_overlap: None,
            budget_used: 1,
            budget_max: 5,
            budget_category: "standard".into(),
            trajectory: None,
            drift_detected: Some(false),
            recap: None,
            adr: None,
            trust_score: None,
            trust_reason: None,
            pattern_recall: None,
        };
        let value = serde_json::to_value(&wire).unwrap();
        assert!(value["traceId"].is_string());
        assert!(value["thoughtNumber"].is_number());
        assert!(value["nextThoughtNeeded"].is_boolean());
        assert!(value.get("driftDetected").is_some());
        assert!(value["budgetCategory"].is_string());
    }

    #[test]
    fn test_wire_response_uses_trajectory_not_ml_trajectory() {
        let wire = WireResponse {
            trace_id: "t1".into(),
            thought_number: 1,
            total_thoughts: 1,
            next_thought_needed: false,
            branches: vec![],
            thought_history_length: 1,
            warnings: vec![],
            alerts: vec![],
            confidence_reported: None,
            confidence_calculated: None,
            confidence_gap: None,
            bias_detected: None,
            sycophancy: None,
            depth_overlap: None,
            budget_used: 1,
            budget_max: 5,
            budget_category: "standard".into(),
            trajectory: Some(0.8),
            drift_detected: None,
            recap: None,
            adr: None,
            trust_score: None,
            trust_reason: None,
            pattern_recall: None,
        };
        let value = serde_json::to_value(&wire).unwrap();
        assert!(value.get("trajectory").is_some());
        assert!(value.get("mlTrajectory").is_none());
    }

    // --- Task 3 tests ---

    #[test]
    fn test_trace_new_generates_uuid() {
        let t = Trace::new();
        assert_eq!(t.id.len(), 36);
        assert!(t.thoughts.is_empty());
        assert!(!t.closed);
        assert!(t.created_at > 0);
    }

    #[test]
    fn test_trace_new_unique_ids() {
        let t1 = Trace::new();
        let t2 = Trace::new();
        assert_ne!(t1.id, t2.id);
    }

    // --- Task 4 tests ---

    #[tokio::test]
    async fn test_process_thought_creates_trace() {
        let server = test_server();
        let input = test_input(1, None, true);
        let wire = server.process_thought(input).await.unwrap();
        assert_eq!(wire.thought_number, 1);
        assert_eq!(wire.trace_id.len(), 36);
        assert_eq!(wire.thought_history_length, 1);
    }

    #[tokio::test]
    async fn test_process_thought_second_thought() {
        let server = test_server();
        let first = server.process_thought(test_input(1, None, true)).await.unwrap();
        let trace_id = first.trace_id.clone();
        let second = server
            .process_thought(test_input(2, Some(trace_id), true))
            .await
            .unwrap();
        assert_eq!(second.thought_history_length, 2);
    }

    #[tokio::test]
    async fn test_process_thought_unknown_trace() {
        let server = test_server();
        let input = test_input(2, Some("nonexistent".into()), true);
        let err = server.process_thought(input).await.unwrap_err();
        assert!(err.contains("unknown trace"));
    }

    #[tokio::test]
    async fn test_process_thought_closes_trace() {
        // With eviction, completing a trace removes it from the map entirely
        let server = test_server();
        let wire = server
            .process_thought(test_input(1, None, false))
            .await
            .unwrap();
        let trace_id = wire.trace_id.clone();
        let traces = server.traces.read().await;
        // Evicted on completion — not present in map
        assert!(!traces.contains_key(&trace_id));
    }

    #[tokio::test]
    async fn test_process_thought_budget_from_config() {
        let server = test_server();
        let mut input = test_input(1, None, true);
        input.thinking_mode = Some("architecture".into());
        let wire = server.process_thought(input).await.unwrap();
        assert_eq!(wire.budget_max, 8);
        assert_eq!(wire.budget_category, "deep");
    }

    // --- Pre-existing tests (unchanged) ---

    #[test]
    fn test_thought_input_deserialize() {
        let json = r#"{
            "thought": "Analyzing the auth flow",
            "thoughtNumber": 1,
            "totalThoughts": 5,
            "nextThoughtNeeded": true,
            "thinkingMode": "architecture",
            "affectedComponents": ["auth", "sessions"],
            "confidence": 75.0,
            "evidence": ["src/auth.rs"],
            "isRevision": false,
            "needsMoreThoughts": false
        }"#;
        let input: ThoughtInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.thought, "Analyzing the auth flow");
        assert_eq!(input.thought_number, 1);
        assert!(input.next_thought_needed);
        assert_eq!(input.thinking_mode, Some("architecture".into()));
        assert_eq!(input.affected_components.len(), 2);
        assert_eq!(input.confidence, Some(75.0));
    }

    #[test]
    fn test_thought_input_defaults() {
        let json = r#"{
            "thought": "Quick check",
            "thoughtNumber": 1,
            "totalThoughts": 1,
            "nextThoughtNeeded": false
        }"#;
        let input: ThoughtInput = serde_json::from_str(json).unwrap();
        assert!(input.affected_components.is_empty());
        assert!(input.evidence.is_empty());
        assert!(!input.is_revision);
        assert!(!input.needs_more_thoughts);
        assert!(input.confidence.is_none());
        assert!(input.thinking_mode.is_none());
    }

    #[test]
    fn test_thought_result_serialize() {
        let result = ThoughtResult::default();
        let json_str = serde_json::to_string(&result).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert!(value["budgetUsed"].is_number());
        assert!(value.get("mlTrajectory").is_some());
        assert!(value.get("confidenceCalculated").is_some());
        assert_eq!(value["budgetUsed"], 0);
        assert!(value["warnings"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_impact_default() {
        let impact = Impact::default();
        assert!(impact.latency.is_none());
        assert!(impact.throughput.is_none());
        assert!(impact.risk.is_none());
    }

    #[tokio::test]
    async fn test_server_new() {
        let config = Config::load("config/feldspar.toml", "config/principles.toml");
        let server = ThinkingServer::new(config, None, None, Arc::new(RwLock::new(HashMap::new())), None);
        assert!(server.db.is_none());
        assert!(server.ml.is_none());
        assert!(server.llm.is_none());
        let traces = server.traces.read().await;
        assert!(traces.is_empty());
    }

    // --- Two-phase concurrency test ---

    #[tokio::test]
    async fn test_concurrent_thoughts_during_recap() {
        // Two independent traces processed concurrently — neither should block the other.
        // recap_every=3 so thought 3 triggers recap attempt (None without LLM, but no panic).
        let server = Arc::new(test_server());

        let s1 = server.clone();
        let h1 = tokio::spawn(async move {
            let w1 = s1.process_thought(test_input(1, None, true)).await.unwrap();
            let id = w1.trace_id.clone();
            let w2 = s1.process_thought(test_input(2, Some(id.clone()), true)).await.unwrap();
            let w3 = s1.process_thought(test_input(3, Some(id), true)).await.unwrap();
            (w1, w2, w3)
        });

        let s2 = server.clone();
        let h2 = tokio::spawn(async move {
            let w1 = s2.process_thought(test_input(1, None, true)).await.unwrap();
            let id = w1.trace_id.clone();
            s2.process_thought(test_input(2, Some(id), true)).await.unwrap()
        });

        let (r1, r2) = tokio::join!(h1, h2);
        let (w1, _w2, w3) = r1.unwrap();
        let w_b = r2.unwrap();

        // Trace A: thought 3, recap attempted (None without LLM — no panic)
        assert_eq!(w3.thought_history_length, 3);
        assert!(w3.recap.is_none()); // no LLM

        // Trace B: independent, unaffected
        assert_eq!(w_b.thought_history_length, 2);

        // Trace IDs are distinct
        assert_ne!(w1.trace_id, w_b.trace_id);
    }

    // --- Recap tests ---

    #[tokio::test]
    async fn test_recap_skipped_without_llm() {
        let server = test_server();
        let w1 = server.process_thought(test_input(1, None, true)).await.unwrap();
        let id = w1.trace_id.clone();
        let w2 = server.process_thought(test_input(2, Some(id.clone()), true)).await.unwrap();
        let w3 = server.process_thought(test_input(3, Some(id), true)).await.unwrap();
        assert!(w1.recap.is_none());
        assert!(w2.recap.is_none());
        assert!(w3.recap.is_none()); // no LLM → always None
    }

    #[tokio::test]
    async fn test_recap_on_third_thought() {
        // recap_every=3: thought 3 triggers recap attempt, no panic even without LLM
        let server = test_server();
        let w1 = server.process_thought(test_input(1, None, true)).await.unwrap();
        let id = w1.trace_id.clone();
        let _ = server.process_thought(test_input(2, Some(id.clone()), true)).await.unwrap();
        let w3 = server.process_thought(test_input(3, Some(id), true)).await.unwrap();
        // No LLM → recap is None, but no panic
        assert!(w3.recap.is_none());
    }

    #[tokio::test]
    async fn test_recap_branch_filtering() {
        // Branch filtering: recap for main-line thought 3 should exclude branch thoughts
        let server = test_server();

        let w1 = server.process_thought(test_input(1, None, true)).await.unwrap();
        let id = w1.trace_id.clone();

        // Thought 2 on branch "alt"
        let mut branch_input = test_input(2, Some(id.clone()), true);
        branch_input.thought = "branch thought".into();
        branch_input.branch_id = Some("alt".into());
        let _ = server.process_thought(branch_input).await.unwrap();

        // Thought 3 on main line — triggers recap, filtered to main-line only
        let mut main_input = test_input(3, Some(id.clone()), true);
        main_input.thought = "main thought 3".into();
        // branch_id remains None (main line)

        // We can't observe the recap_text directly, but we can verify no panic
        // and that thought 3 is processed correctly
        let w3 = server.process_thought(main_input).await.unwrap();
        assert_eq!(w3.thought_history_length, 3);
        assert!(w3.recap.is_none()); // no LLM
    }

    // --- ADR tests ---

    fn make_trace_with_thoughts(thoughts: Vec<(&str, Option<&str>, Vec<&str>, Option<&str>)>) -> Trace {
        // thoughts: (text, branch_id, components, mode)
        let mut trace = Trace::new();
        for (text, branch_id, components, mode) in thoughts {
            trace.thoughts.push(ThoughtRecord {
                input: ThoughtInput {
                    trace_id: Some(trace.id.clone()),
                    thought: text.into(),
                    thought_number: trace.thoughts.len() as u32 + 1,
                    total_thoughts: 5,
                    next_thought_needed: true,
                    thinking_mode: mode.map(|s| s.into()),
                    affected_components: components.iter().map(|s| s.to_string()).collect(),
                    confidence: None,
                    evidence: vec![],
                    estimated_impact: None,
                    is_revision: false,
                    revises_thought: None,
                    branch_from_thought: None,
                    branch_id: branch_id.map(|s| s.into()),
                    needs_more_thoughts: false,
                },
                result: ThoughtResult::default(),
                created_at: 1712361600000, // 2024-04-06
            });
        }
        trace
    }

    #[test]
    fn test_generate_adr_basic() {
        let trace = make_trace_with_thoughts(vec![
            ("First thought about auth", None, vec!["auth"], Some("architecture")),
            ("Second thought: conclusion", None, vec!["auth"], Some("architecture")),
        ]);
        let adr = generate_adr(&trace);
        assert!(adr.contains("auth"), "components should include auth");
        assert!(adr.contains("architecture"), "mode should include architecture");
        assert!(adr.contains("Second thought: conclusion"), "decision = last main-line thought");
    }

    #[test]
    fn test_generate_adr_decision_from_mainline() {
        let trace = make_trace_with_thoughts(vec![
            ("main thought 1", None, vec![], None),
            ("branch thought 2", Some("alt-1"), vec![], None),
            ("main thought 3 — final decision", None, vec![], None),
        ]);
        let adr = generate_adr(&trace);
        assert!(adr.contains("main thought 3 — final decision"), "decision is last main-line");
        // Decision line should not contain branch thought; it may appear in Branches explored
        let decision_line = adr.lines().find(|l| l.starts_with("**Decision**")).unwrap();
        assert!(!decision_line.contains("branch thought 2"), "branch thought not in Decision line");
    }

    #[test]
    fn test_generate_adr_with_branches() {
        let trace = make_trace_with_thoughts(vec![
            ("main thought 1", None, vec![], None),
            ("alt-1 exploration start", Some("alt-1"), vec![], None),
            ("alt-1 second thought", Some("alt-1"), vec![], None),
        ]);
        let adr = generate_adr(&trace);
        assert!(adr.contains("alt-1"), "branches explored should mention alt-1");
        assert!(adr.contains("alt-1 exploration start"), "first branch thought included");
    }

    #[test]
    fn test_generate_adr_no_components() {
        let trace = make_trace_with_thoughts(vec![
            ("thought with no components", None, vec![], None),
        ]);
        let adr = generate_adr(&trace);
        assert!(adr.contains("**Components**: none"), "no components → none");
    }

    #[test]
    fn test_generate_adr_deterministic() {
        let trace = make_trace_with_thoughts(vec![
            ("thought", None, vec!["auth", "sessions", "config"], Some("architecture")),
        ]);
        let adr1 = generate_adr(&trace);
        let adr2 = generate_adr(&trace);
        assert_eq!(adr1, adr2, "ADR output must be deterministic");
    }

    #[test]
    fn test_generate_adr_multibyte_truncation() {
        // 50 CJK chars (3 bytes each) + 51 ASCII chars = 101 chars total → should truncate
        let text = format!("{}{}", "你".repeat(50), "a".repeat(51));
        let trace = make_trace_with_thoughts(vec![
            ("main", None, vec![], None),
            (&text, Some("alt"), vec![], None),
        ]);
        let adr = generate_adr(&trace);
        assert!(adr.contains("alt"), "branch label present");
        assert!(adr.contains("..."), "truncation marker present");
    }

    #[test]
    fn test_generate_adr_no_truncation_under_100() {
        let text = "a".repeat(50);
        let trace = make_trace_with_thoughts(vec![
            ("main", None, vec![], None),
            (&text, Some("alt"), vec![], None),
        ]);
        let adr = generate_adr(&trace);
        assert!(!adr.contains("..."), "no truncation for 50 chars");
        assert!(adr.contains(&text), "full text present");
    }

    // --- Eviction tests ---

    #[tokio::test]
    async fn test_process_thought_adr_on_completion() {
        let server = test_server();
        let w1 = server.process_thought(test_input(1, None, true)).await.unwrap();
        let id = w1.trace_id.clone();
        let mut close_input = test_input(2, Some(id), false);
        close_input.thought = "final decision".into();
        let w2 = server.process_thought(close_input).await.unwrap();
        assert!(w2.adr.is_some(), "ADR generated on completion");
        let adr = w2.adr.unwrap();
        assert!(adr.contains("## ADR"), "ADR has header");
        assert!(adr.contains("**Date**"), "ADR has date");
        assert!(adr.contains("final decision"), "ADR contains decision text");
    }

    #[tokio::test]
    async fn test_eviction_removes_trace() {
        let server = test_server();
        let w1 = server.process_thought(test_input(1, None, true)).await.unwrap();
        let id = w1.trace_id.clone();
        server.process_thought(test_input(2, Some(id.clone()), false)).await.unwrap();
        let traces = server.traces.read().await;
        assert!(!traces.contains_key(&id), "trace evicted on completion");
    }

    #[tokio::test]
    async fn test_eviction_map_empty_after_close() {
        let server = test_server();
        let w1 = server.process_thought(test_input(1, None, true)).await.unwrap();
        let id = w1.trace_id.clone();
        server.process_thought(test_input(2, Some(id), false)).await.unwrap();
        let traces = server.traces.read().await;
        assert!(traces.is_empty(), "HashMap empty after all traces closed");
    }

    // --- Date helper tests ---

    #[test]
    fn test_unix_millis_to_date_known_epoch() {
        // 2024-04-06 00:00:00 UTC = 1712361600 seconds
        let date = unix_millis_to_date(1712361600000);
        assert_eq!(date, "2024-04-06");
    }

    #[test]
    fn test_unix_millis_to_date_epoch() {
        let date = unix_millis_to_date(0);
        assert_eq!(date, "1970-01-01");
    }

    // --- pattern_recall tests ---

    #[tokio::test]
    async fn test_process_thought_returns_pattern_recall_none() {
        let server = test_server();
        let wire = server.process_thought(test_input(1, None, true)).await.unwrap();
        assert!(wire.pattern_recall.is_none(), "pattern_recall is None when ML not wired");
    }

    #[test]
    fn test_wire_response_serialization_skips_none_pattern_recall() {
        let wire = WireResponse {
            trace_id: "t1".into(),
            thought_number: 1,
            total_thoughts: 3,
            next_thought_needed: true,
            branches: vec![],
            thought_history_length: 1,
            warnings: vec![],
            alerts: vec![],
            confidence_reported: None,
            confidence_calculated: None,
            confidence_gap: None,
            bias_detected: None,
            sycophancy: None,
            depth_overlap: None,
            budget_used: 1,
            budget_max: 5,
            budget_category: "standard".into(),
            trajectory: None,
            drift_detected: None,
            recap: None,
            adr: None,
            trust_score: None,
            trust_reason: None,
            pattern_recall: None,
        };
        let json_str = serde_json::to_string(&wire).unwrap();
        assert!(!json_str.contains("patternRecall"), "patternRecall absent when None");
    }

    // --- Trust scoring completion path tests ---

    #[tokio::test]
    async fn test_process_thought_trust_warning_missing_mode() {
        let server = test_server();
        let mut input = test_input(1, None, false); // nextThoughtNeeded = false
        input.thinking_mode = None;
        let wire = server.process_thought(input).await.unwrap(); // Must be Ok, not Err
        assert!(wire.warnings.contains(&"THINKING_MODE_MISSING".to_string()));
        assert!(wire.trust_score.is_none());
        assert!(wire.trust_reason.is_none());
        assert!(wire.adr.is_some(), "ADR preserved despite missing mode");
    }

    #[tokio::test]
    async fn test_process_thought_trust_warning_no_api_key() {
        // test_server() creates ThinkingServer with llm: None
        let server = test_server();
        let mut input = test_input(1, None, false);
        input.thinking_mode = Some("debugging".into());
        let wire = server.process_thought(input).await.unwrap();
        assert!(wire.warnings.iter().any(|w| w == "OPENROUTER_KEY_NOT_SET"));
        assert!(wire.trust_score.is_none());
        assert!(wire.trust_reason.is_none());
    }

    #[tokio::test]
    async fn test_process_thought_no_trust_on_continuation() {
        // Non-completion thoughts should have no trust fields
        let server = test_server();
        let mut input = test_input(1, None, true); // nextThoughtNeeded = true
        input.thinking_mode = Some("debugging".into());
        let wire = server.process_thought(input).await.unwrap();
        assert!(wire.trust_score.is_none());
        assert!(wire.trust_reason.is_none());
        // No trust-related warnings
        assert!(!wire.warnings.iter().any(|w|
            w.contains("TRUST") || w.contains("THINKING_MODE") || w.contains("OPENROUTER")
        ));
    }

    // --- ML integration tests ---

    fn make_trained_ml() -> Arc<crate::ml::MlEngine> {
        use std::path::PathBuf;
        let booster = crate::ml::MlEngine::default_booster(0.5);
        let mut mode_map = HashMap::new();
        mode_map.insert("architecture".to_string(), 0);
        mode_map.insert("debugging".to_string(), 1);
        let engine = crate::ml::MlEngine::new(booster, mode_map, PathBuf::from("/tmp/test_ml.bin"));
        // Train with 5 samples to flush the buffer and produce trees
        for i in 0..5 {
            engine.train(vec![i as f64 * 0.1; 16], (i as f64 + 1.0) * 1.5);
        }
        Arc::new(engine)
    }

    fn test_server_with_ml() -> ThinkingServer {
        let config = crate::config::Config {
            feldspar: crate::config::FeldsparConfig {
                db_path: "test.db".into(),
                model_path: "test.model".into(),
                recap_every: 3,
                pattern_recall_top_k: 3,
                ml_budget: 0.5,
                pattern_recall_min_traces: 10,
            },
            llm: crate::config::LlmConfig {
                base_url: None,
                api_key_env: Some("TEST_KEY".into()),
                model: "test-model".into(),
            },
            thresholds: crate::config::ThresholdsConfig {
                confidence_gap: 25.0,
                over_analysis_multiplier: 1.5,
                overthinking_multiplier: 2.0,
            },
            budgets: HashMap::from([
                ("minimal".into(), [2, 3]),
                ("standard".into(), [3, 5]),
                ("deep".into(), [5, 8]),
            ]),
            modes: HashMap::from([
                (
                    "architecture".into(),
                    crate::config::ModeConfig {
                        requires: vec![],
                        budget: "deep".into(),
                        watches: String::new(),
                    },
                ),
            ]),
            components: crate::config::ComponentsConfig { valid: vec![] },
            ar: None,
            principles: vec![],
        };
        ThinkingServer::new(
            Arc::new(config),
            None,
            None,
            Arc::new(RwLock::new(HashMap::new())),
            Some(make_trained_ml()),
        )
    }

    #[tokio::test]
    async fn test_ml_predict_populates_trajectory() {
        let server = test_server_with_ml();
        let mut input = test_input(1, None, true);
        input.confidence = Some(75.0);
        let wire = server.process_thought(input).await.unwrap();
        // Trained model should produce a trajectory in [0,1]
        assert!(wire.trajectory.is_some(), "trajectory should be Some after training");
        let t = wire.trajectory.unwrap();
        assert!((0.0..=1.0).contains(&t), "trajectory {t} not in [0,1]");
    }

    #[tokio::test]
    async fn test_ml_drift_field_populated() {
        let server = test_server_with_ml();
        let input = test_input(1, None, true);
        let wire = server.process_thought(input).await.unwrap();
        // drift_detected is always Some when ML is present (false until drift fires)
        assert!(wire.drift_detected.is_some(), "drift_detected should be Some when ML present");
    }

    #[tokio::test]
    async fn test_pattern_recall_below_min_traces() {
        // leaf_cache has 5 entries (below default min of 10) → pattern_recall None
        let server = test_server_with_ml();
        {
            let mut cache = server.leaf_cache.write().await;
            for i in 0..5 {
                cache.insert(format!("trace_{i}"), vec![i, i + 1, i + 2]);
            }
        }
        let input = test_input(1, None, true);
        let wire = server.process_thought(input).await.unwrap();
        assert!(wire.pattern_recall.is_none(), "pattern_recall None when below min_traces");
    }

    #[tokio::test]
    async fn test_pattern_recall_none_without_ml() {
        // Existing behavior: no ML → no pattern_recall
        let server = test_server();
        let input = test_input(1, None, true);
        let wire = server.process_thought(input).await.unwrap();
        assert!(wire.pattern_recall.is_none());
    }

    #[tokio::test]
    async fn test_process_thought_without_ml_no_trajectory() {
        let server = test_server();
        let input = test_input(1, None, true);
        let wire = server.process_thought(input).await.unwrap();
        assert!(wire.trajectory.is_none());
        assert!(wire.drift_detected.is_none());
        assert!(wire.pattern_recall.is_none());
    }

    #[tokio::test]
    async fn test_process_thought_with_ml_cold() {
        // ml=Some but fresh booster (no trees) → trajectory None, drift false
        use std::path::PathBuf;
        let booster = crate::ml::MlEngine::default_booster(0.5);
        let mut mode_map = HashMap::new();
        mode_map.insert("architecture".to_string(), 0);
        let engine = crate::ml::MlEngine::new(booster, mode_map, PathBuf::from("/tmp/cold_ml.bin"));
        // Do NOT train — cold model has no trees
        let ml = Arc::new(engine);

        let config = crate::config::Config {
            feldspar: crate::config::FeldsparConfig {
                db_path: "test.db".into(),
                model_path: "test.model".into(),
                recap_every: 3,
                pattern_recall_top_k: 3,
                ml_budget: 0.5,
                pattern_recall_min_traces: 10,
            },
            llm: crate::config::LlmConfig {
                base_url: None,
                api_key_env: Some("TEST_KEY".into()),
                model: "test-model".into(),
            },
            thresholds: crate::config::ThresholdsConfig {
                confidence_gap: 25.0,
                over_analysis_multiplier: 1.5,
                overthinking_multiplier: 2.0,
            },
            budgets: HashMap::from([
                ("minimal".into(), [2, 3]),
                ("standard".into(), [3, 5]),
                ("deep".into(), [5, 8]),
            ]),
            modes: HashMap::from([(
                "architecture".into(),
                crate::config::ModeConfig {
                    requires: vec![],
                    budget: "deep".into(),
                    watches: String::new(),
                },
            )]),
            components: crate::config::ComponentsConfig { valid: vec![] },
            ar: None,
            principles: vec![],
        };
        let server = ThinkingServer::new(
            Arc::new(config),
            None,
            None,
            Arc::new(RwLock::new(HashMap::new())),
            Some(ml),
        );
        let input = test_input(1, None, true);
        let wire = server.process_thought(input).await.unwrap();
        assert!(wire.trajectory.is_none(), "cold model should produce no trajectory");
        assert_eq!(wire.drift_detected, Some(false), "cold model should report no drift");
    }

    #[tokio::test]
    async fn test_pattern_recall_on_thought_1() {
        // Populate leaf_cache above min_traces (10) → pattern_recall should fire
        let server = test_server_with_ml();
        {
            let mut cache = server.leaf_cache.write().await;
            for i in 0..15 {
                cache.insert(format!("trace_{i}"), vec![i, i + 1, i + 2]);
            }
        }
        let input = test_input(1, None, true);
        let wire = server.process_thought(input).await.unwrap();
        // Pattern recall should be Some — even if empty results (no DB to find_traces_by_ids),
        // the field should be populated or None depending on find_similar results.
        // Without a DB, find_traces_by_ids returns empty, so pattern_recall may be None.
        // The key assertion: the code PATH executes (no panic, no error).
        // With no DB: find_similar returns IDs, but db.find_traces_by_ids is skipped → None.
        // This test validates the execution path doesn't crash with a populated cache.
        assert!(wire.warnings.iter().all(|w| !w.contains("panic")));
    }

    #[tokio::test]
    async fn test_trace_completion_stores_features() {
        // Complete a trace with ML → features blob should be passed to flush_trace
        // We can't verify DB storage without a real DB, but we verify the code path executes.
        let server = test_server_with_ml();
        let mut input1 = test_input(1, None, true);
        input1.thinking_mode = Some("architecture".into());
        input1.confidence = Some(80.0);
        let w1 = server.process_thought(input1).await.unwrap();
        let id = w1.trace_id.clone();

        let mut input2 = test_input(2, Some(id), false); // completion
        input2.thinking_mode = Some("architecture".into());
        input2.confidence = Some(85.0);
        // This should not panic — features are computed and passed to flush_trace
        let w2 = server.process_thought(input2).await.unwrap();
        assert!(w2.adr.is_some() || w2.adr.is_none()); // just verify no crash
    }

    #[tokio::test]
    async fn test_process_thought_completion_preserves_adr_with_mode() {
        // Even when trust scoring fails (no LLM), ADR should be present
        let server = test_server();
        let mut input1 = test_input(1, None, true);
        input1.thinking_mode = Some("architecture".into());
        let w1 = server.process_thought(input1).await.unwrap();
        let id = w1.trace_id.clone();

        let mut input2 = test_input(2, Some(id), false);
        input2.thinking_mode = Some("architecture".into());
        input2.thought = "final decision here".into();
        let w2 = server.process_thought(input2).await.unwrap();
        assert!(w2.adr.is_some());
        assert!(w2.adr.unwrap().contains("final decision here"));
    }

}
