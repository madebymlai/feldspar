// PerpetualBooster ML: real-time per-thought inference + autonomous learning.
//
// No hardcoded outcome formula. Raw signals as features, PerpetualBooster auto-learns importance.
// One global model, thinking_mode as feature column. Handles categoricals natively.
//
// Signals (all as feature columns):
//   Process (per thought):  warning responsiveness, confidence convergence, depth progression.
//   Outcome (per trace):    trust score from trace review.
//
// predict(features) -> f64: every thought, microsecond inference, 0-1 trajectory score.
// drift(features) -> DriftReport: every thought, flags data/concept drift.
// train(features): on trace completion. Incremental O(n).
//
// Pattern recall via leaf matching (PerpetualBooster's predict_nodes):
//   On trace completion: store leaf node set (HashSet<usize>) in DB.
//   On thought 1: predict_nodes() on current features → compare via Jaccard similarity
//   against stored past traces. Higher overlap = more similar by learned decision boundaries.
//   Returns top-K similar traces for formatting into patternRecall response.
//
// On startup: bulk train from DB history. Model file saved to disk as backup.
// Best-effort: all calls wrapped in Option return, failures return None, never block.

use crate::thought::ThoughtInput;
use perpetual::data::Matrix;
use perpetual::drift::calculate_drift;
use perpetual::objective::Objective;
use perpetual::PerpetualBooster;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, RwLock};
use tracing::warn;

pub const MODEL_SCHEMA_VERSION: u32 = 1;

// Number of features in the feature vector.
const FEATURE_COUNT: usize = 16;

// Train buffer flush threshold.
const TRAIN_FLUSH_AT: usize = 5;

// Max drift sample window.
const DRIFT_WINDOW_MAX: usize = 50;

// Drift check period (thoughts).
const DRIFT_CHECK_PERIOD: usize = 10;

// Minimum samples needed before drift check fires.
const DRIFT_MIN_SAMPLES: usize = 10;

// Prune cycle minimum trace count.
const PRUNE_MIN_TRACES: usize = 100;

// Leaf-set removal threshold for trace eviction.
const PRUNE_EVICT_THRESHOLD: f64 = 0.80;

// trust_score normalization divisor.
const TRUST_SCORE_SCALE: f64 = 10.0;

/// Aggregated trace state passed to extract_features.
/// Constructed by thought.rs from Trace data before calling ML.
pub struct TraceSnapshot {
    pub thought_count: u32,
    pub avg_confidence: f64,
    pub avg_confidence_gap: f64,
    pub avg_prior_depth: f64,
    pub current_depth_overlap: f64,
    pub branch_count: usize,
    pub revision_count: usize,
    pub budget_used: u32,
    pub budget_max: u32,
    pub prior_warning_count: usize,
    pub warning_responsiveness_ratio: f64, // NaN if 0 warnings
    pub confidence_convergence: f64,        // NaN if <4 thoughts
}

/// Summary of drift check.
pub struct DriftReport {
    pub data_drift: bool,
    pub concept_drift: bool,
}

// ---------------------------------------------------------------------------
// DriftTracker
// ---------------------------------------------------------------------------

struct DriftTracker {
    samples: Vec<Vec<f64>>,
    data_scores: VecDeque<f32>,
    concept_scores: VecDeque<f32>,
    thoughts_since_check: usize,
}

impl DriftTracker {
    fn new() -> Self {
        Self {
            samples: Vec::new(),
            data_scores: VecDeque::new(),
            concept_scores: VecDeque::new(),
            thoughts_since_check: 0,
        }
    }

    fn record(&mut self, features: Vec<f64>) {
        self.samples.push(features);
        if self.samples.len() > DRIFT_WINDOW_MAX {
            self.samples.remove(0);
        }
        self.thoughts_since_check += 1;
    }

    fn ready(&self) -> bool {
        self.thoughts_since_check >= DRIFT_CHECK_PERIOD && self.samples.len() >= DRIFT_MIN_SAMPLES
    }

    fn check(&mut self, booster: &PerpetualBooster) -> DriftReport {
        self.thoughts_since_check = 0;

        let col_major = transpose_to_column_major(&self.samples);
        let n = self.samples.len();
        let matrix = Matrix::new(&col_major, n, FEATURE_COUNT);

        let data_score = calculate_drift(booster, &matrix, "data", false);
        let concept_score = calculate_drift(booster, &matrix, "concept", false);

        push_score(&mut self.data_scores, data_score);
        push_score(&mut self.concept_scores, concept_score);

        DriftReport {
            data_drift: self.data_scores.len() >= DRIFT_MIN_SAMPLES
                && has_variance(&self.data_scores)
                && data_score > percentile_95(&self.data_scores),
            concept_drift: self.concept_scores.len() >= DRIFT_MIN_SAMPLES
                && has_variance(&self.concept_scores)
                && concept_score > percentile_95(&self.concept_scores),
        }
    }
}

fn push_score(window: &mut VecDeque<f32>, score: f32) {
    window.push_back(score);
    if window.len() > DRIFT_WINDOW_MAX {
        window.pop_front();
    }
}

// ---------------------------------------------------------------------------
// PruneGuard — RAII reset for pruning AtomicBool
// ---------------------------------------------------------------------------

struct PruneGuard<'a>(&'a AtomicBool);

impl Drop for PruneGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// MlEngine
// ---------------------------------------------------------------------------

pub struct MlEngine {
    model: RwLock<PerpetualBooster>,
    drift_tracker: Mutex<DriftTracker>,
    train_buffer: Mutex<Vec<(Vec<f64>, f64)>>,
    pub mode_map: HashMap<String, usize>,
    model_path: PathBuf,
    max_nodes: AtomicUsize,
    pruning: AtomicBool,
}

impl MlEngine {
    pub fn new(
        booster: PerpetualBooster,
        mode_map: HashMap<String, usize>,
        model_path: PathBuf,
    ) -> Self {
        let max_nodes = Self::compute_max_nodes(&booster);
        Self {
            model: RwLock::new(booster),
            drift_tracker: Mutex::new(DriftTracker::new()),
            train_buffer: Mutex::new(Vec::new()),
            mode_map,
            model_path,
            max_nodes: AtomicUsize::new(max_nodes),
            pruning: AtomicBool::new(false),
        }
    }

    pub fn default_booster(ml_budget: f64) -> PerpetualBooster {
        PerpetualBooster::default()
            .set_objective(Objective::AdaptiveHuberLoss { quantile: Some(0.5) })
            .set_reset(Some(false))
            .set_save_node_stats(true)
            .set_create_missing_branch(true)
            .set_budget(ml_budget as f32)
            .set_categorical_features(Some(HashSet::from([0])))
    }

    pub fn load(
        path: &Path,
        mode_map: HashMap<String, usize>,
        ml_budget: f64,
    ) -> Option<Self> {
        let data = std::fs::read(path).ok()?;
        if data.len() < 4 {
            warn!("model file too small: {} bytes", data.len());
            return None;
        }
        let version = u32::from_le_bytes(data[..4].try_into().ok()?);
        if version != MODEL_SCHEMA_VERSION {
            warn!(
                "model schema version mismatch: got {}, expected {}",
                version, MODEL_SCHEMA_VERSION
            );
            return None;
        }
        let booster: PerpetualBooster = serde_json::from_slice(&data[4..]).ok()?;
        let _ = ml_budget; // budget stored in model; parameter unused after load
        Some(Self::new(booster, mode_map, path.to_path_buf()))
    }

    /// Reconstruct a model from feature matrix loaded from DB.
    /// Used when the model file is missing but DB has historical trace data.
    pub fn disaster_recover(
        matrix: &[(Vec<f64>, f64)],
        mode_map: HashMap<String, usize>,
        model_path: PathBuf,
        ml_budget: f64,
    ) -> Option<Self> {
        if matrix.is_empty() {
            return None;
        }
        let booster = Self::default_booster(ml_budget);
        let engine = Self::new(booster, mode_map, model_path);
        // Train in batches matching the flush threshold
        for chunk in matrix.chunks(TRAIN_FLUSH_AT) {
            engine.train_batch(chunk);
        }
        Some(engine)
    }

    pub fn save(&self) -> Option<()> {
        let model = self.model.read().ok()?;
        save_model_atomic(&model, &self.model_path)
    }

    fn compute_max_nodes(booster: &PerpetualBooster) -> usize {
        booster
            .get_prediction_trees()
            .iter()
            .map(|t| t.nodes.len())
            .max()
            .unwrap_or(0)
    }

    // -------------------------------------------------------------------------
    // predict
    // -------------------------------------------------------------------------

    pub fn predict(&self, features: &[f64]) -> Option<f64> {
        let model = self.model.read().ok()?;
        if model.get_prediction_trees().is_empty() {
            return None;
        }
        let matrix = Matrix::new(features, 1, features.len());
        let preds = model.predict(&matrix, false);
        preds.first().map(|&v: &f64| v.clamp(0.0, 1.0))
    }

    // -------------------------------------------------------------------------
    // extract_features — pure function, 16-element vector, NaN for undefined
    // -------------------------------------------------------------------------

    pub fn extract_features(
        input: &ThoughtInput,
        snapshot: &TraceSnapshot,
        mode_map: &HashMap<String, usize>,
    ) -> Vec<f64> {
        let mode_idx = input
            .thinking_mode
            .as_ref()
            .and_then(|m| mode_map.get(m))
            .map(|&i| i as f64)
            .unwrap_or(f64::NAN);

        let depth_prog = if snapshot.thought_count < 2 {
            f64::NAN
        } else if snapshot.avg_prior_depth == 0.0 {
            f64::NAN // guard against +inf
        } else {
            snapshot.current_depth_overlap / snapshot.avg_prior_depth
        };

        vec![
            mode_idx,                                                          // 0: thinking_mode
            input.affected_components.len() as f64,                           // 1: component_count
            input.confidence.unwrap_or(f64::NAN),                             // 2: confidence_reported
            input.evidence.len() as f64,                                       // 3: evidence_count
            if input.branch_from_thought.is_some() { 1.0 } else { 0.0 },     // 4: has_branch
            if input.is_revision { 1.0 } else { 0.0 },                       // 5: has_revision
            snapshot.prior_warning_count as f64,                               // 6: warning_count (prior)
            if snapshot.thought_count < 2 { f64::NAN } else { snapshot.avg_confidence }, // 7
            if snapshot.thought_count < 2 { f64::NAN } else { snapshot.avg_confidence_gap }, // 8
            snapshot.branch_count as f64,                                      // 9
            snapshot.revision_count as f64,                                    // 10
            input.thought_number as f64 / input.total_thoughts as f64,        // 11: thought_progress
            snapshot.budget_used as f64 / snapshot.budget_max.max(1) as f64,  // 12: budget_ratio
            snapshot.warning_responsiveness_ratio,                              // 13: NaN if no warnings
            if snapshot.thought_count < 4 { f64::NAN } else { snapshot.confidence_convergence }, // 14
            depth_prog,                                                         // 15: depth_progression_ratio
        ]
    }

    // -------------------------------------------------------------------------
    // drift
    // -------------------------------------------------------------------------

    pub fn drift(&self, features: &[f64]) -> DriftReport {
        let mut tracker = match self.drift_tracker.lock() {
            Ok(t) => t,
            Err(_) => {
                return DriftReport {
                    data_drift: false,
                    concept_drift: false,
                }
            }
        };
        tracker.record(features.to_vec());
        if !tracker.ready() {
            return DriftReport {
                data_drift: false,
                concept_drift: false,
            };
        }
        let model: std::sync::RwLockReadGuard<PerpetualBooster> = match self.model.read() {
            Ok(m) => m,
            Err(_) => {
                return DriftReport {
                    data_drift: false,
                    concept_drift: false,
                }
            }
        };
        if model.get_prediction_trees().is_empty() {
            return DriftReport {
                data_drift: false,
                concept_drift: false,
            };
        }
        tracker.check(&model)
    }

    // -------------------------------------------------------------------------
    // train (buffered)
    // -------------------------------------------------------------------------

    pub fn train(&self, features: Vec<f64>, trust_score: f64) {
        let target = trust_score / TRUST_SCORE_SCALE;
        let mut buffer = match self.train_buffer.lock() {
            Ok(b) => b,
            Err(_) => return,
        };
        buffer.push((features, target));
        if buffer.len() >= TRAIN_FLUSH_AT {
            let batch: Vec<(Vec<f64>, f64)> = buffer.drain(..).collect();
            drop(buffer); // release mutex before acquiring write lock
            self.train_batch(&batch);
        }
    }

    /// Trains on any remaining buffered samples. Call on session end.
    pub fn flush_buffer(&self) {
        let mut buffer = match self.train_buffer.lock() {
            Ok(b) => b,
            Err(_) => return,
        };
        if buffer.is_empty() {
            return;
        }
        let batch: Vec<(Vec<f64>, f64)> = buffer.drain(..).collect();
        drop(buffer);
        self.train_batch(&batch);
    }

    fn train_batch(&self, batch: &[(Vec<f64>, f64)]) {
        let rows: Vec<&Vec<f64>> = batch.iter().map(|(f, _)| f).collect();
        let y: Vec<f64> = batch.iter().map(|(_, t)| *t).collect();
        let col_major = transpose_to_column_major_refs(&rows);

        let trained = {
            let mut model: std::sync::RwLockWriteGuard<PerpetualBooster> =
                match self.model.write() {
                    Ok(m) => m,
                    Err(_) => return,
                };
            // PerpetualBooster may panic on degenerate data (all-uniform features).
            // Catch it to keep the server alive; the lock guard drops on unwind.
            let fit_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                model.fit(
                    &Matrix::new(&col_major, batch.len(), FEATURE_COUNT),
                    &y,
                    None,
                    None,
                )
            }));
            match fit_result {
                Ok(Ok(())) => {
                    self.max_nodes
                        .store(Self::compute_max_nodes(&model), Ordering::Relaxed);
                    true
                }
                Ok(Err(e)) => {
                    warn!(error = %e, "perpetual fit returned error");
                    false
                }
                Err(_) => {
                    warn!("perpetual fit panicked on degenerate training batch — skipping");
                    false
                }
            }
        }; // write lock released here

        if trained {
            let _ = self.save();
        }
    }

    // -------------------------------------------------------------------------
    // predict_nodes + find_similar
    // -------------------------------------------------------------------------

    pub fn predict_nodes(&self, features: &[f64]) -> Option<Vec<usize>> {
        let mn = self.max_nodes.load(Ordering::Relaxed);
        if mn == 0 {
            return None;
        }
        let model = self.model.read().ok()?;
        let matrix = Matrix::new(features, 1, features.len());
        let raw = model.predict_nodes(&matrix, false);
        Some(flatten_leaf_sets(raw, mn))
    }

    pub fn find_similar(
        &self,
        features: &[f64],
        leaf_cache: &HashMap<String, Vec<usize>>,
        top_k: usize,
    ) -> Vec<String> {
        let current = match self.predict_nodes(features) {
            Some(leaves) => leaves.into_iter().collect::<HashSet<usize>>(),
            None => return Vec::new(),
        };
        if leaf_cache.is_empty() {
            return Vec::new();
        }

        let mut scored: Vec<(String, f64)> = leaf_cache
            .iter()
            .map(|(id, leaves)| (id.clone(), jaccard(&current, leaves)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().take(top_k).map(|(id, _)| id).collect()
    }

    // -------------------------------------------------------------------------
    // prune_cycle
    // -------------------------------------------------------------------------

    /// Returns trace IDs to evict. Caller handles DB + cache eviction.
    pub fn prune_cycle(
        &self,
        feature_matrix: &[(Vec<f64>, f64)],
        leaf_cache: &RwLock<HashMap<String, Vec<usize>>>,
    ) -> Vec<String> {
        if self
            .pruning
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Vec::new();
        }
        let _guard = PruneGuard(&self.pruning);

        if feature_matrix.len() < PRUNE_MIN_TRACES {
            return Vec::new();
        }

        let before = self.snapshot_all_nodes();

        {
            let mut model: std::sync::RwLockWriteGuard<PerpetualBooster> =
                match self.model.write() {
                    Ok(m) => m,
                    Err(_) => return Vec::new(),
                };
            let rows: Vec<&Vec<f64>> = feature_matrix.iter().map(|(f, _)| f).collect();
            let y: Vec<f64> = feature_matrix.iter().map(|(_, t)| *t).collect();
            let col_major = transpose_to_column_major_refs(&rows);
            let matrix = Matrix::new(&col_major, feature_matrix.len(), FEATURE_COUNT);
            let _ = model.prune(&matrix, &y, None, None);
            self.max_nodes
                .store(Self::compute_max_nodes(&model), Ordering::Relaxed);
        }

        let after = self.snapshot_all_nodes();
        let removed: HashSet<usize> = before.difference(&after).copied().collect();
        if removed.is_empty() {
            let _ = self.save();
            return Vec::new();
        }

        let cache = match leaf_cache.read() {
            Ok(c) => c,
            Err(_) => {
                let _ = self.save();
                return Vec::new();
            }
        };
        let mut evict_ids = Vec::new();
        for (trace_id, leaves) in cache.iter() {
            let removed_count = leaves.iter().filter(|l| removed.contains(l)).count();
            if leaves.is_empty()
                || (removed_count as f64 / leaves.len() as f64) > PRUNE_EVICT_THRESHOLD
            {
                evict_ids.push(trace_id.clone());
            }
        }

        let _ = self.save();
        evict_ids
    }

    fn snapshot_all_nodes(&self) -> HashSet<usize> {
        let model: std::sync::RwLockReadGuard<PerpetualBooster> = match self.model.read() {
            Ok(m) => m,
            Err(_) => return HashSet::new(),
        };
        let mn = self.max_nodes.load(Ordering::Relaxed);
        let mut nodes = HashSet::new();
        for (tree_idx, tree) in model.get_prediction_trees().iter().enumerate() {
            for &node_id in tree.nodes.keys() {
                nodes.insert(tree_idx * mn + node_id);
            }
        }
        nodes
    }
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

fn save_model_atomic(booster: &PerpetualBooster, path: &Path) -> Option<()> {
    let tmp = path.with_extension("bin.tmp");
    let mut data = MODEL_SCHEMA_VERSION.to_le_bytes().to_vec();
    data.extend(serde_json::to_vec(booster).ok()?);
    std::fs::write(&tmp, &data).ok()?;
    let f = std::fs::File::open(&tmp).ok()?;
    f.sync_all().ok()?;
    std::fs::rename(&tmp, path).ok()?;
    Some(())
}

fn flatten_leaf_sets(raw: Vec<Vec<HashSet<usize>>>, max_nodes: usize) -> Vec<usize> {
    let mut flat = HashSet::new();
    for (tree_idx, tree_samples) in raw.iter().enumerate() {
        if let Some(sample) = tree_samples.first() {
            for &node_id in sample {
                flat.insert(tree_idx * max_nodes + node_id);
            }
        }
    }
    flat.into_iter().collect()
}

fn jaccard(a: &HashSet<usize>, b: &[usize]) -> f64 {
    let b_set: HashSet<usize> = b.iter().copied().collect();
    let intersection = a.intersection(&b_set).count();
    let union = a.len() + b_set.len() - intersection;
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

fn transpose_to_column_major(rows: &[Vec<f64>]) -> Vec<f64> {
    if rows.is_empty() {
        return Vec::new();
    }
    let n = rows.len();
    let m = rows[0].len();
    let mut col_major = vec![0.0_f64; n * m];
    for col in 0..m {
        for row in 0..n {
            col_major[col * n + row] = rows[row][col];
        }
    }
    col_major
}

fn transpose_to_column_major_refs(rows: &[&Vec<f64>]) -> Vec<f64> {
    if rows.is_empty() {
        return Vec::new();
    }
    let n = rows.len();
    let m = rows[0].len();
    let mut col_major = vec![0.0_f64; n * m];
    for col in 0..m {
        for row in 0..n {
            col_major[col * n + row] = rows[row][col];
        }
    }
    col_major
}

fn percentile_95(window: &VecDeque<f32>) -> f32 {
    let mut sorted: Vec<f32> = window.iter().copied().collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((sorted.len() as f64) * 0.95).floor() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn has_variance(window: &VecDeque<f32>) -> bool {
    if window.len() < 2 {
        return false;
    }
    let first = window[0];
    window.iter().any(|&v| (v - first).abs() > f32::EPSILON)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::NamedTempFile;

    // -------------------------------------------------------------------------
    // Helpers
    // -------------------------------------------------------------------------

    fn default_mode_map() -> HashMap<String, usize> {
        let mut m = HashMap::new();
        m.insert("architecture".to_string(), 0);
        m.insert("debugging".to_string(), 1);
        m
    }

    fn make_engine(path: PathBuf) -> MlEngine {
        let booster = MlEngine::default_booster(0.5);
        MlEngine::new(booster, default_mode_map(), path)
    }

    fn dummy_input(thought_number: u32) -> ThoughtInput {
        ThoughtInput {
            trace_id: None,
            thought: "test".to_string(),
            thought_number,
            total_thoughts: 5,
            next_thought_needed: true,
            thinking_mode: Some("architecture".to_string()),
            affected_components: vec!["core".to_string()],
            confidence: Some(75.0),
            evidence: vec!["file.rs".to_string()],
            estimated_impact: None,
            is_revision: false,
            revises_thought: None,
            branch_from_thought: None,
            branch_id: None,
            needs_more_thoughts: false,
        }
    }

    fn dummy_snapshot(thought_count: u32) -> TraceSnapshot {
        TraceSnapshot {
            thought_count,
            avg_confidence: 70.0,
            avg_confidence_gap: 5.0,
            avg_prior_depth: 0.5,
            current_depth_overlap: 0.4,
            branch_count: 0,
            revision_count: 0,
            budget_used: 3,
            budget_max: 10,
            prior_warning_count: 0,
            warning_responsiveness_ratio: f64::NAN,
            confidence_convergence: f64::NAN,
        }
    }

    fn make_16_features(val: f64) -> Vec<f64> {
        vec![val; FEATURE_COUNT]
    }

    /// Train engine with synthetic data so it has trees.
    fn train_engine(engine: &MlEngine) {
        for i in 0..5 {
            let features = make_16_features(i as f64 * 0.1);
            engine.train(features, (i as f64 + 1.0) * 2.0);
        }
    }

    // -------------------------------------------------------------------------
    // Task 1: MlEngine struct + construction + save/load
    // -------------------------------------------------------------------------


    #[test]
    fn test_default_booster_settings() {
        let b = MlEngine::default_booster(0.5);
        // Verify observable settings via constructor chain — booster is valid (no panic)
        // reset=false, save_node_stats=true, create_missing_branch=true are set via builder
        // We can't easily introspect all fields; verify it compiles and constructs.
        let _ = b;
    }

    #[test]
    fn test_save_load_roundtrip() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();
        let engine = make_engine(path.clone());
        train_engine(&engine);

        engine.save().expect("save should succeed");

        let features = make_16_features(0.2);
        let pred_before = engine.predict(&features);

        let engine2 = MlEngine::load(&path, default_mode_map(), 0.5).expect("load should succeed");
        let pred_after = engine2.predict(&features);

        assert!(pred_before.is_some());
        assert!(pred_after.is_some());
        // Predictions should be within float tolerance
        let diff = (pred_before.unwrap() - pred_after.unwrap()).abs();
        assert!(diff < 1e-6, "predictions differ after roundtrip: {diff}");
    }

    #[test]
    fn test_load_version_mismatch() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();
        // Write wrong version
        let wrong_version: u32 = 99;
        std::fs::write(&path, wrong_version.to_le_bytes()).unwrap();
        assert!(MlEngine::load(&path, default_mode_map(), 0.5).is_none());
    }

    #[test]
    fn test_load_corrupted_file() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();
        // Write correct version + garbage
        let mut data = MODEL_SCHEMA_VERSION.to_le_bytes().to_vec();
        data.extend_from_slice(b"not valid bincode garbage data here");
        std::fs::write(&path, &data).unwrap();
        assert!(MlEngine::load(&path, default_mode_map(), 0.5).is_none());
    }

    #[test]
    fn test_load_empty_file() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();
        std::fs::write(&path, b"").unwrap();
        assert!(MlEngine::load(&path, default_mode_map(), 0.5).is_none());
    }

    #[test]
    fn test_save_atomic_creates_no_tmp() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();
        let engine = make_engine(path.clone());
        train_engine(&engine);
        engine.save().expect("save should succeed");
        let tmp = path.with_extension("bin.tmp");
        assert!(!tmp.exists(), ".bin.tmp file should not remain after save");
    }

    #[test]
    fn test_compute_max_nodes_empty_model() {
        let booster = MlEngine::default_booster(0.5);
        let nodes = MlEngine::new(booster, HashMap::new(), PathBuf::from("/tmp/test.bin"))
            .max_nodes
            .load(Ordering::Relaxed);
        assert_eq!(nodes, 0);
    }

    // -------------------------------------------------------------------------
    // Task 2: predict + extract_features
    // -------------------------------------------------------------------------

    #[test]
    fn test_predict_cold_start_returns_none() {
        let engine = make_engine(PathBuf::from("/tmp/cold_start_test.bin"));
        let features = make_16_features(0.5);
        assert!(engine.predict(&features).is_none());
    }

    #[test]
    fn test_predict_returns_clamped() {
        let file = NamedTempFile::new().unwrap();
        let engine = make_engine(file.path().to_path_buf());
        train_engine(&engine);
        let features = make_16_features(0.3);
        let pred = engine.predict(&features).expect("should have prediction after training");
        assert!((0.0..=1.0).contains(&pred), "prediction {pred} not in [0,1]");
    }

    #[test]
    fn test_extract_features_length() {
        let input = dummy_input(2);
        let snapshot = dummy_snapshot(2);
        let features = MlEngine::extract_features(&input, &snapshot, &default_mode_map());
        assert_eq!(features.len(), FEATURE_COUNT);
    }

    #[test]
    fn test_extract_features_thought_1_nans() {
        let input = dummy_input(1);
        let snapshot = TraceSnapshot {
            thought_count: 1,
            ..dummy_snapshot(1)
        };
        let features = MlEngine::extract_features(&input, &snapshot, &default_mode_map());
        // features 7, 8, 13, 14, 15 should be NaN
        for idx in [7, 8, 13, 14, 15] {
            assert!(
                features[idx].is_nan(),
                "feature[{idx}] should be NaN for thought_count=1, got {}",
                features[idx]
            );
        }
    }

    #[test]
    fn test_extract_features_depth_zero_nan() {
        let input = dummy_input(3);
        let snapshot = TraceSnapshot {
            thought_count: 3,
            avg_prior_depth: 0.0,
            ..dummy_snapshot(3)
        };
        let features = MlEngine::extract_features(&input, &snapshot, &default_mode_map());
        assert!(
            features[15].is_nan(),
            "feature[15] should be NaN when avg_prior_depth==0, got {}",
            features[15]
        );
    }

    #[test]
    fn test_extract_features_unknown_mode() {
        let mut input = dummy_input(2);
        input.thinking_mode = Some("unknown_mode".to_string());
        let snapshot = dummy_snapshot(2);
        let features = MlEngine::extract_features(&input, &snapshot, &default_mode_map());
        assert!(
            features[0].is_nan(),
            "feature[0] should be NaN for unknown mode"
        );
    }

    #[test]
    fn test_extract_features_budget_ratio_no_div_zero() {
        let input = dummy_input(2);
        let snapshot = TraceSnapshot {
            thought_count: 2,
            budget_max: 0,
            ..dummy_snapshot(2)
        };
        let features = MlEngine::extract_features(&input, &snapshot, &default_mode_map());
        // budget_max=0 → uses max(1), so budget_ratio = 3/1 = 3.0 (not infinity)
        assert!(features[12].is_finite(), "feature[12] should be finite when budget_max=0");
    }

    // -------------------------------------------------------------------------
    // Task 3: DriftTracker
    // -------------------------------------------------------------------------

    #[test]
    fn test_drift_tracker_not_ready_under_10() {
        let mut tracker = DriftTracker::new();
        for _ in 0..5 {
            tracker.record(make_16_features(0.5));
        }
        assert!(!tracker.ready());
    }

    #[test]
    fn test_drift_tracker_ready_at_10() {
        let mut tracker = DriftTracker::new();
        for _ in 0..10 {
            tracker.record(make_16_features(0.5));
        }
        assert!(tracker.ready());
    }

    #[test]
    fn test_drift_tracker_check_resets_counter() {
        let file = NamedTempFile::new().unwrap();
        let engine = make_engine(file.path().to_path_buf());
        train_engine(&engine);

        let mut tracker = DriftTracker::new();
        for _ in 0..10 {
            tracker.record(make_16_features(0.5));
        }
        assert!(tracker.ready());

        let model = engine.model.read().unwrap();
        tracker.check(&model);
        assert!(!tracker.ready());
    }

    #[test]
    fn test_drift_cold_model_no_drift() {
        let engine = make_engine(PathBuf::from("/tmp/cold_drift_test.bin"));
        // Record enough samples via the public interface but cold model → no drift
        for _ in 0..10 {
            let report = engine.drift(&make_16_features(0.5));
            // Should never flag drift with no trees
            assert!(!report.data_drift);
            assert!(!report.concept_drift);
        }
    }

    #[test]
    fn test_drift_all_zero_no_spurious_flag() {
        let mut window: VecDeque<f32> = VecDeque::new();
        for _ in 0..15 {
            window.push_back(0.0);
        }
        assert!(!has_variance(&window));
    }

    #[test]
    fn test_drift_window_capped_at_50() {
        let mut tracker = DriftTracker::new();
        for _ in 0..60 {
            tracker.record(make_16_features(0.5));
        }
        assert_eq!(tracker.samples.len(), DRIFT_WINDOW_MAX);
    }

    // -------------------------------------------------------------------------
    // Task 4: train + predict_nodes + find_similar + Jaccard
    // -------------------------------------------------------------------------

    #[test]
    fn test_train_buffer_accumulates() {
        let engine = make_engine(PathBuf::from("/tmp/buf_test.bin"));
        // Push only 3 — should not flush
        for i in 0..3 {
            engine.train(make_16_features(i as f64 * 0.1), 5.0);
        }
        // Model should still have no trees (cold start — no flush yet)
        assert!(engine.predict(&make_16_features(0.1)).is_none());
    }

    #[test]
    fn test_train_buffer_flushes_at_5() {
        let file = NamedTempFile::new().unwrap();
        let engine = make_engine(file.path().to_path_buf());
        train_engine(&engine); // pushes exactly 5
        assert!(engine.max_nodes.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn test_flush_buffer_trains_remaining() {
        let file = NamedTempFile::new().unwrap();
        let engine = make_engine(file.path().to_path_buf());
        // Push only 3 (no auto-flush)
        for i in 0..3 {
            engine.train(make_16_features(i as f64 * 0.1), 5.0);
        }
        engine.flush_buffer();
        // After flush, model may or may not produce trees (3 samples might be enough)
        // The important thing is no panic and buffer is empty
        let buffer = engine.train_buffer.lock().unwrap();
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_predict_nodes_cold_start_none() {
        let engine = make_engine(PathBuf::from("/tmp/nodes_cold.bin"));
        assert!(engine.predict_nodes(&make_16_features(0.5)).is_none());
    }

    #[test]
    fn test_predict_nodes_returns_vec() {
        let file = NamedTempFile::new().unwrap();
        let engine = make_engine(file.path().to_path_buf());
        train_engine(&engine);
        let nodes = engine.predict_nodes(&make_16_features(0.3));
        assert!(nodes.is_some());
    }

    #[test]
    fn test_flatten_leaf_sets_tree_prefix() {
        // tree 0 → nodes {1, 2}, tree 1 → nodes {3}, max_nodes = 10
        // expected: {0*10+1, 0*10+2, 1*10+3} = {1, 2, 13}
        let mut set0 = HashSet::new();
        set0.insert(1usize);
        set0.insert(2usize);
        let mut set1 = HashSet::new();
        set1.insert(3usize);
        let raw: Vec<Vec<HashSet<usize>>> = vec![vec![set0], vec![set1]];
        let mut result = flatten_leaf_sets(raw, 10);
        result.sort();
        assert_eq!(result, vec![1, 2, 13]);
    }

    #[test]
    fn test_jaccard_identical() {
        let a: HashSet<usize> = [1, 2, 3].iter().copied().collect();
        let b = vec![1, 2, 3];
        assert!((jaccard(&a, &b) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_jaccard_disjoint() {
        let a: HashSet<usize> = [1, 2].iter().copied().collect();
        let b = vec![3, 4];
        assert!((jaccard(&a, &b) - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_jaccard_partial() {
        let a: HashSet<usize> = [1, 2, 3].iter().copied().collect();
        let b = vec![2, 3, 4];
        // intersection=2, union=4
        let expected = 2.0 / 4.0;
        assert!((jaccard(&a, &b) - expected).abs() < 1e-10);
    }

    #[test]
    fn test_jaccard_empty() {
        let a: HashSet<usize> = HashSet::new();
        let b: Vec<usize> = vec![];
        assert!((jaccard(&a, &b) - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_transpose_column_major() {
        // [[1,2],[3,4]] → column-major → [1,3,2,4]
        let rows = vec![vec![1.0_f64, 2.0], vec![3.0, 4.0]];
        let result = transpose_to_column_major(&rows);
        assert_eq!(result, vec![1.0, 3.0, 2.0, 4.0]);
    }

    #[test]
    fn test_find_similar_returns_top_k() {
        let file = NamedTempFile::new().unwrap();
        let engine = make_engine(file.path().to_path_buf());
        train_engine(&engine);

        let mut cache: HashMap<String, Vec<usize>> = HashMap::new();
        for i in 0..10 {
            // Store leaf sets (just dummy indices)
            cache.insert(format!("trace_{i}"), vec![i, i + 1, i + 2]);
        }

        let result = engine.find_similar(&make_16_features(0.2), &cache, 3);
        assert_eq!(result.len(), 3);
    }

    // -------------------------------------------------------------------------
    // Task 5: prune_cycle + percentile/variance helpers
    // -------------------------------------------------------------------------

    #[test]
    fn test_prune_guard_resets_flag() {
        let flag = AtomicBool::new(false);
        {
            flag.store(true, Ordering::SeqCst);
            let _guard = PruneGuard(&flag);
        }
        assert!(!flag.load(Ordering::SeqCst));
    }

    #[test]
    fn test_prune_concurrent_dedup() {
        let file = NamedTempFile::new().unwrap();
        let engine = make_engine(file.path().to_path_buf());
        engine.pruning.store(true, Ordering::SeqCst);
        let cache: RwLock<HashMap<String, Vec<usize>>> = RwLock::new(HashMap::new());
        let result = engine.prune_cycle(&[], &cache);
        assert!(result.is_empty());
        // Reset so engine can be dropped cleanly
        engine.pruning.store(false, Ordering::SeqCst);
    }

    #[test]
    fn test_prune_under_100_noop() {
        let file = NamedTempFile::new().unwrap();
        let engine = make_engine(file.path().to_path_buf());
        let matrix: Vec<(Vec<f64>, f64)> = (0..50)
            .map(|i| (make_16_features(i as f64 * 0.01), 0.5))
            .collect();
        let cache: RwLock<HashMap<String, Vec<usize>>> = RwLock::new(HashMap::new());
        let result = engine.prune_cycle(&matrix, &cache);
        assert!(result.is_empty());
    }

    #[test]
    fn test_percentile_95_known() {
        let window: VecDeque<f32> = (1..=10).map(|i| i as f32).collect();
        // 10 elements, idx = floor(10 * 0.95) = 9 → sorted[9] = 10.0
        assert!((percentile_95(&window) - 10.0).abs() < 1e-5);
    }

    #[test]
    fn test_percentile_95_single() {
        let mut window = VecDeque::new();
        window.push_back(5.0_f32);
        assert!((percentile_95(&window) - 5.0).abs() < 1e-5);
    }

    #[test]
    fn test_has_variance_identical() {
        let window: VecDeque<f32> = vec![3.0, 3.0, 3.0].into_iter().collect();
        assert!(!has_variance(&window));
    }

    #[test]
    fn test_has_variance_different() {
        let window: VecDeque<f32> = vec![3.0, 3.0, 3.1].into_iter().collect();
        assert!(has_variance(&window));
    }

    #[test]
    fn test_has_variance_empty() {
        let window: VecDeque<f32> = VecDeque::new();
        assert!(!has_variance(&window));
    }
}
