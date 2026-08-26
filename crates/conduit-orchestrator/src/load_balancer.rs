//! LoadBalancer — select the executing channel from the candidate list, with
//! retry / failover orchestration (RUST-P9-004, steps S04–S10).
//!
//! Ported from the Go orchestrator:
//! - `conduit/internal/server/orchestrator/load_balancer.go` (`LoadBalancer.Sort`,
//!   `sortProduction`/`sortWithDebug`, `calculateTopK`, the `OrderingWeight`
//!   tie-break, debug breakdown).
//! - `conduit/internal/server/orchestrator/retry.go` (`isRetryableError`,
//!   `isRetryableErrorForChannel`, `matchesRetryableErrorPattern`,
//!   `ExtractStatusCodeFromError`, `deriveLoadBalancerStrategy`).
//! - `conduit/internal/server/orchestrator/outbound.go` (`NextChannel`,
//!   `PrepareForRetry`, `CanRetry` — the same-channel / next-channel retry
//!   advancement contract).
//! - `conduit/internal/server/biz/system.go` (`RetryPolicy`, the
//!   `LoadBalancerStrategy*` constants, `defaultRetryPolicy`,
//!   `normalizeRetryPolicy`'s `weighted → failover` rewrite).
//!
//! The strategies themselves (`RoundRobin`/`Weight`/`TraceAware`/`ErrorAware`/
//! `LatencyAware`/`CircuitBreaker`/`RateLimit`/`Quota`) are external services in
//! Go (they read live metrics). This module captures the **pure logic** that
//! composes their scores into an ordered candidate list plus the failover state
//! machine — fully unit-testable with fixtures. The strategy provider is a
//! trait so tests inject deterministic scores.

use crate::candidate::Candidate;
use conduit_core::objects::channel_settings::RetryableErrorPattern;
use conduit_llm::RequestType;
use regex::Regex;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub trait LoadBalancer {
    fn select<'a>(&mut self, candidates: &'a [Candidate]) -> Option<&'a Candidate>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebugScore {
    pub strategy: String,
    pub candidate_id: String,
    pub components: Vec<ScoreComponent>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScoreComponent {
    pub name: String,
    pub value: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StrategyBreakdown {
    pub scores: Vec<DebugScore>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadBalancerSelection<'a> {
    pub candidate: Option<&'a Candidate>,
    pub breakdown: Option<StrategyBreakdown>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryTopKPolicy {
    pub enabled: bool,
    pub max_channel_retries: u32,
}

impl RetryTopKPolicy {
    pub const fn new(enabled: bool, max_channel_retries: u32) -> Self {
        Self {
            enabled,
            max_channel_retries,
        }
    }

    pub const fn top_k(self) -> usize {
        if self.enabled {
            1 + self.max_channel_retries as usize
        } else {
            1
        }
    }
}

pub fn failover_attempt_order(candidates: &[Candidate], top_k: usize) -> Vec<&Candidate> {
    candidates.iter().take(top_k).collect()
}

#[derive(Clone, Debug, Default)]
pub struct WeightedRoundRobin {
    entries: Vec<WeightedRoundRobinEntry>,
}

impl WeightedRoundRobin {
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    fn current_weight_for(&self, candidate_id: &str) -> i64 {
        self.entries
            .iter()
            .find(|entry| entry.candidate_id == candidate_id)
            .map_or(0, |entry| entry.current_weight)
    }

    fn sync_entries(&mut self, candidates: &[Candidate]) {
        self.entries.retain(|entry| {
            candidates
                .iter()
                .any(|candidate| candidate.id == entry.candidate_id)
        });

        for candidate in candidates {
            if self
                .entries
                .iter()
                .all(|entry| entry.candidate_id != candidate.id)
            {
                self.entries.push(WeightedRoundRobinEntry {
                    candidate_id: candidate.id.clone(),
                    current_weight: 0,
                });
            }
        }
    }

    pub fn select_with_debug<'a>(
        &mut self,
        candidates: &'a [Candidate],
        debug: bool,
    ) -> LoadBalancerSelection<'a> {
        self.select_internal(candidates, debug)
    }

    fn select_internal<'a>(
        &mut self,
        candidates: &'a [Candidate],
        debug: bool,
    ) -> LoadBalancerSelection<'a> {
        self.sync_entries(candidates);

        let total_weight: i64 = candidates
            .iter()
            .filter(|candidate| candidate.weight > 0)
            .map(|candidate| i64::from(candidate.weight))
            .sum();

        if total_weight == 0 {
            return LoadBalancerSelection {
                candidate: None,
                breakdown: debug.then_some(StrategyBreakdown::default()),
            };
        }

        let mut scores = debug.then(Vec::new);

        for entry in &mut self.entries {
            if let Some(candidate) = candidates
                .iter()
                .find(|candidate| candidate.id == entry.candidate_id && candidate.weight > 0)
            {
                let previous_weight = entry.current_weight;
                entry.current_weight += i64::from(candidate.weight);

                if let Some(scores) = &mut scores {
                    scores.push(DebugScore {
                        strategy: "weighted_round_robin".to_string(),
                        candidate_id: candidate.id.clone(),
                        components: vec![
                            ScoreComponent {
                                name: "previous_current_weight".to_string(),
                                value: previous_weight,
                            },
                            ScoreComponent {
                                name: "effective_weight".to_string(),
                                value: i64::from(candidate.weight),
                            },
                            ScoreComponent {
                                name: "current_weight".to_string(),
                                value: entry.current_weight,
                            },
                            ScoreComponent {
                                name: "total_weight".to_string(),
                                value: total_weight,
                            },
                        ],
                    });
                }
            }
        }

        let selected_index = candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate.weight > 0)
            .max_by(|(left_index, left), (right_index, right)| {
                // Prefer higher accumulated weight; preserve input order on ties.
                let left_weight = self.current_weight_for(&left.id);
                let right_weight = self.current_weight_for(&right.id);
                left_weight.cmp(&right_weight).then_with(|| {
                    compare_ordering_weight_tie_break(
                        OrderingWeightTieBreak::new(0, *left_index),
                        OrderingWeightTieBreak::new(0, *right_index),
                    )
                })
            })
            .map(|(index, _)| index);

        if let Some(selected_index) = selected_index
            && let Some(entry) = self
                .entries
                .iter_mut()
                .find(|entry| entry.candidate_id == candidates[selected_index].id)
        {
            entry.current_weight -= total_weight;
        }

        LoadBalancerSelection {
            candidate: selected_index.map(|index| &candidates[index]),
            breakdown: scores.map(|scores| StrategyBreakdown { scores }),
        }
    }
}

impl LoadBalancer for WeightedRoundRobin {
    fn select<'a>(&mut self, candidates: &'a [Candidate]) -> Option<&'a Candidate> {
        self.select_internal(candidates, false).candidate
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RoundRobinBucketKey {
    pub model: String,
    pub request_type: RequestType,
}

impl RoundRobinBucketKey {
    pub fn new(model: impl Into<String>, request_type: RequestType) -> Self {
        Self {
            model: model.into(),
            request_type,
        }
    }
}

#[derive(Debug, Default)]
pub struct BucketedRoundRobin {
    buckets: Mutex<HashMap<RoundRobinBucketKey, WeightedRoundRobin>>,
}

impl BucketedRoundRobin {
    pub fn new() -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
        }
    }

    pub fn select<'a>(
        &self,
        model: impl Into<String>,
        request_type: RequestType,
        candidates: &'a [Candidate],
    ) -> Option<&'a Candidate> {
        let key = RoundRobinBucketKey::new(model, request_type);
        // A poisoned mutex means a prior panicking select left the bucket map
        // inconsistent; treat it as "no candidate available" rather than
        // panicking the whole load balancer.
        let mut buckets = self.buckets.lock().ok()?;

        buckets.entry(key).or_default().select(candidates)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OrderingWeightTieBreak {
    ordering_weight: i64,
    input_index: usize,
}

impl OrderingWeightTieBreak {
    const fn new(ordering_weight: i64, input_index: usize) -> Self {
        Self {
            ordering_weight,
            input_index,
        }
    }
}

fn compare_ordering_weight_tie_break(
    left: OrderingWeightTieBreak,
    right: OrderingWeightTieBreak,
) -> Ordering {
    // Do not use candidate/channel ID here; equal weights preserve caller input order.
    left.ordering_weight
        .cmp(&right.ordering_weight)
        .then_with(|| right.input_index.cmp(&left.input_index))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WeightedRoundRobinEntry {
    candidate_id: String,
    current_weight: i64,
}

// ===========================================================================
// Strategy / scoring (S04) — pure-logic composition of external strategy scores
// ===========================================================================

/// A scoring strategy for a single candidate. Mirrors Go `LoadBalanceStrategy`:
/// the production path returns only the score; the debug path returns a
/// [`ScoreComponent`] breakdown for diagnostics.
///
/// `Send + Sync` so `Arc<dyn ScoringStrategy>` held by `CommandOrchestrator`
/// keeps the orchestrator `Sync` (required to wire it into async handlers).
pub trait ScoringStrategy: Send + Sync {
    /// Strategy identifier (mirrors Go `LoadBalanceStrategy.Name`).
    fn name(&self) -> &'static str;

    /// Production score. Higher = higher priority. Range mirrors Go (0..=1000
    /// for most strategies; the trace boost is 1000).
    fn score(&self, candidate: &Candidate) -> i64;

    /// Debug breakdown. Default delegates to [`Self::score`] with a single
    /// component; strategies with internal detail override.
    fn score_with_debug(&self, candidate: &Candidate) -> (i64, Vec<ScoreComponent>) {
        let value = self.score(candidate);
        (
            value,
            vec![ScoreComponent {
                name: self.name().to_string(),
                value,
            }],
        )
    }

    /// Observe the channel that actually became the first attempt. Stateful
    /// strategies (adaptive weighted distribution) use this to update their
    /// next-request score; stateless failover/circuit strategies are no-ops.
    fn record_selection(&self, _candidate: &Candidate) {}
}

/// Runtime adaptive scorer using the channel's configured ordering weight and
/// an in-process selection count. This is the live core of Go's
/// `WeightRoundRobinStrategy`: channels with the smallest
/// `selection_count / weight` rank first, so long-running traffic converges to
/// the configured proportions. Other adaptive signals remain composable via
/// [`CompositeScoring`] as their live providers are wired.
#[derive(Debug, Default)]
pub struct AdaptiveWeightedScoring {
    selections: Mutex<HashMap<String, u64>>,
}

impl AdaptiveWeightedScoring {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn selection_count(&self, candidate_id: &str) -> u64 {
        self.selections
            .lock()
            .ok()
            .and_then(|counts| counts.get(candidate_id).copied())
            .unwrap_or(0)
    }
}

impl ScoringStrategy for AdaptiveWeightedScoring {
    fn name(&self) -> &'static str {
        "AdaptiveWeighted"
    }

    fn score(&self, candidate: &Candidate) -> i64 {
        let count = self.selection_count(&candidate.id);
        // Go treats weight <= 0 as equal standard round-robin weight.
        let weight = if candidate.ordering_weight > 0 {
            candidate.ordering_weight
        } else {
            100
        };
        let normalized = (i128::from(count) * 100_000) / i128::from(weight);
        -i64::try_from(normalized).unwrap_or(i64::MAX)
    }

    fn record_selection(&self, candidate: &Candidate) {
        if let Ok(mut counts) = self.selections.lock() {
            let count = counts.entry(candidate.id.clone()).or_insert(0);
            *count = count.saturating_add(1);
        }
    }
}

/// A static-score strategy (mirrors Go `WeightStrategy`: score = ordering_weight
/// normalized to 0..=100). Used by the `failover` and `circuit-breaker` load
/// balancers as the deterministic priority signal.
#[derive(Clone, Copy, Debug, Default)]
pub struct WeightScoring {
    max_score: i64,
}

impl WeightScoring {
    pub const fn new() -> Self {
        Self { max_score: 100 }
    }
}

impl ScoringStrategy for WeightScoring {
    fn name(&self) -> &'static str {
        "Weight"
    }

    fn score(&self, candidate: &Candidate) -> i64 {
        // weight is typically 0..=100; clamp negatives, scale to max_score.
        let weight = candidate.ordering_weight.max(0);
        // Assume max weight is 100, scale accordingly (Go `WeightStrategy.Score`).
        (weight * self.max_score) / 100
    }
}

/// Adds a request-normalized theoretical procurement-cost component to an
/// existing strategy. A weight of 0 preserves the base strategy exactly;
/// 100 gives the cheapest candidate up to 1000 additional score points.
pub struct CostAwareScoring {
    base: Arc<dyn ScoringStrategy>,
    weight_percent: i64,
}

impl CostAwareScoring {
    pub fn new(base: Arc<dyn ScoringStrategy>, weight_percent: i64) -> Self {
        Self {
            base,
            weight_percent: weight_percent.clamp(0, 100),
        }
    }

    fn cost_component(&self, candidate: &Candidate) -> i64 {
        candidate.cost_efficiency_score * self.weight_percent / 100
    }
}

impl ScoringStrategy for CostAwareScoring {
    fn name(&self) -> &'static str {
        "CostAware"
    }

    fn score(&self, candidate: &Candidate) -> i64 {
        self.base.score(candidate) + self.cost_component(candidate)
    }

    fn score_with_debug(&self, candidate: &Candidate) -> (i64, Vec<ScoreComponent>) {
        let (base_score, mut components) = self.base.score_with_debug(candidate);
        let cost_score = self.cost_component(candidate);
        components.push(ScoreComponent {
            name: "theoretical_cost_efficiency".to_string(),
            value: cost_score,
        });
        (base_score + cost_score, components)
    }

    fn record_selection(&self, candidate: &Candidate) {
        self.base.record_selection(candidate);
    }
}

/// Trace-aware boost (mirrors Go `TraceAwareStrategy`): returns `boost_score`
/// for the channel matching the sticky/trace hint, 0 otherwise. The provider
/// resolves the "last successful channel for this trace" — here it is supplied
/// directly via [`TraceAwareScoring::new`].
#[derive(Clone, Debug)]
pub struct TraceAwareScoring {
    last_successful_channel_id: Option<String>,
    boost_score: i64,
}

impl TraceAwareScoring {
    pub const DEFAULT_BOOST: i64 = 1000;

    pub fn for_channel(channel_id: impl Into<String>) -> Self {
        Self {
            last_successful_channel_id: Some(channel_id.into()),
            boost_score: Self::DEFAULT_BOOST,
        }
    }
}

impl ScoringStrategy for TraceAwareScoring {
    fn name(&self) -> &'static str {
        "TraceAware"
    }

    fn score(&self, candidate: &Candidate) -> i64 {
        if self
            .last_successful_channel_id
            .as_deref()
            .is_some_and(|id| id == candidate.id)
        {
            self.boost_score
        } else {
            0
        }
    }
}

/// A composite strategy summing the scores of its members, optionally with
/// per-member weights (mirrors Go `CompositeStrategy` /
/// `LoadBalancer.sortProduction`). Note: not `Clone` because the member
/// strategies are trait objects; callers needing diagnostics use
/// [`Self::score_with_debug`].
pub struct CompositeScoring {
    members: Vec<CompositeMember>,
}

struct CompositeMember {
    strategy: Box<dyn ScoringStrategy>,
    weight: f64,
}

impl std::fmt::Debug for CompositeScoring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompositeScoring")
            .field("member_count", &self.members.len())
            .finish()
    }
}

impl Default for CompositeScoring {
    fn default() -> Self {
        Self::new()
    }
}

impl CompositeScoring {
    pub fn new() -> Self {
        Self {
            members: Vec::new(),
        }
    }

    /// Add a member with the default weight of 1.0 (mirrors Go
    /// `NewCompositeStrategy` which sets `weight: 1.0`).
    pub fn with(mut self, strategy: impl ScoringStrategy + 'static) -> Self {
        self.members.push(CompositeMember {
            strategy: Box::new(strategy),
            weight: 1.0,
        });
        self
    }

    /// Add a member with a custom weight (mirrors Go
    /// `compositeStrategyWeight{strategy: s, weight: w}`). The member's score
    /// is multiplied by `weight` in the composite sum.
    pub fn with_weight(mut self, strategy: impl ScoringStrategy + 'static, weight: f64) -> Self {
        self.members.push(CompositeMember {
            strategy: Box::new(strategy),
            weight,
        });
        self
    }

    /// Override the weights of already-added members (mirrors Go
    /// `CompositeStrategy.WithWeights`). Extra weights beyond the member count
    /// are silently dropped (Go: `if i < len(c.strategies)`).
    pub fn with_weights(mut self, weights: &[f64]) -> Self {
        for (member, &weight) in self.members.iter_mut().zip(weights.iter()) {
            member.weight = weight;
        }
        self
    }
}

impl ScoringStrategy for CompositeScoring {
    fn name(&self) -> &'static str {
        "Composite"
    }

    fn score(&self, candidate: &Candidate) -> i64 {
        self.members
            .iter()
            .map(|m| (m.strategy.score(candidate) as f64 * m.weight) as i64)
            .sum()
    }

    fn score_with_debug(&self, candidate: &Candidate) -> (i64, Vec<ScoreComponent>) {
        let mut total = 0i64;
        let mut components = Vec::with_capacity(self.members.len());
        for member in &self.members {
            let (score, debug) = member.strategy.score_with_debug(candidate);
            let weighted = (score as f64 * member.weight) as i64;
            total += weighted;
            // Flatten nested debug into per-strategy components.
            if debug.len() == 1 {
                components.push(debug.into_iter().next().unwrap_or_else(|| ScoreComponent {
                    name: member.strategy.name().to_string(),
                    value: weighted,
                }));
            } else {
                components.extend(debug);
            }
        }
        (total, components)
    }
}

// ===========================================================================
// Candidate sorting (S04/S10) — port of Go LoadBalancer.Sort
// ===========================================================================

/// A scored candidate ready for ordering. Built by [`score_candidates`].
#[derive(Clone, Debug)]
pub struct ScoredCandidate<'a> {
    pub candidate: &'a Candidate,
    pub score: i64,
    /// Original position in the input slice, used as the final tie-break so
    /// equal-score, equal-weight candidates preserve caller order (Go returns
    /// 0 from the comparator for this case, which `partial.Sort` treats as
    /// "leave in input order").
    input_index: usize,
}

/// Score every candidate with `strategy`. Mirrors Go `sortProduction`'s scoring
/// loop (without the partial-sort step).
pub fn score_candidates<'a>(
    candidates: &'a [Candidate],
    strategy: &dyn ScoringStrategy,
) -> Vec<ScoredCandidate<'a>> {
    candidates
        .iter()
        .enumerate()
        .map(|(i, c)| ScoredCandidate {
            candidate: c,
            score: strategy.score(c),
            input_index: i,
        })
        .collect()
}

/// Compare two scored candidates for descending priority. Mirrors Go
/// `partial.SortFunc`: higher score wins; on a tie higher `ordering_weight`
/// wins; on a further tie input order is preserved (return `Equal`, which a
/// stable sort leaves in place — matching Go's deliberate "do NOT use channel
/// ID as tie-breaker to avoid uneven distribution" comment).
pub fn compare_scored(left: &ScoredCandidate, right: &ScoredCandidate) -> Ordering {
    right
        .score
        .cmp(&left.score)
        .then_with(|| {
            right
                .candidate
                .ordering_weight
                .cmp(&left.candidate.ordering_weight)
        })
        .then_with(|| left.input_index.cmp(&right.input_index))
}

/// Sort candidates by descending priority and return the top-`top_k` references
/// in score order. Mirrors Go `LoadBalancer.Sort` (production path): score →
/// ordering_weight tie-break → stable input order. This is the pure-logic
/// equivalent of Go's `partial.SortFunc(scored, topK, ...)`.
pub fn sort_candidates_top_k<'a>(
    candidates: &'a [Candidate],
    strategy: &dyn ScoringStrategy,
    top_k: usize,
) -> Vec<&'a Candidate> {
    if candidates.len() <= 1 {
        return candidates.iter().take(top_k).collect();
    }
    let mut scored = score_candidates(candidates, strategy);
    // Stable sort by the comparator; ties keep input order (matches Go's
    // return-0 behavior under a stable partial sort).
    scored.sort_by(compare_scored);
    scored
        .into_iter()
        .take(top_k)
        .map(|s| s.candidate)
        .collect()
}

/// Debug variant of [`sort_candidates_top_k`] returning the full per-candidate
/// score breakdown (mirrors Go `sortWithDebug`).
pub fn sort_candidates_with_debug<'a>(
    candidates: &'a [Candidate],
    strategy: &dyn ScoringStrategy,
    top_k: usize,
) -> (Vec<&'a Candidate>, StrategyBreakdown) {
    let mut scored: Vec<(usize, i64, Vec<ScoreComponent>)> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let (score, debug) = strategy.score_with_debug(c);
            (i, score, debug)
        })
        .collect();
    scored.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| {
                candidates[right.0]
                    .ordering_weight
                    .cmp(&candidates[left.0].ordering_weight)
            })
            .then_with(|| left.0.cmp(&right.0))
    });

    let breakdown = StrategyBreakdown {
        scores: scored
            .iter()
            .map(|(i, _score, components)| DebugScore {
                strategy: "composite".to_string(),
                candidate_id: candidates[*i].id.clone(),
                components: components.clone(),
            })
            .collect(),
    };
    let result = scored
        .into_iter()
        .take(top_k)
        .map(|(i, _, _)| &candidates[i])
        .collect();
    (result, breakdown)
}

// ===========================================================================
// Retry policy + strategy selection (S04) — port of biz.RetryPolicy
// ===========================================================================

/// Load balancer strategy selector. Mirrors Go's
/// `LoadBalancerStrategyAdaptive` / `Failover` / `CircuitBreaker` constants.
/// `Weighted` is normalized to `Failover` (Go `normalizeRetryPolicy`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadBalancerStrategy {
    Adaptive,
    Failover,
    CircuitBreaker,
}

impl LoadBalancerStrategy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Adaptive => "adaptive",
            Self::Failover => "failover",
            Self::CircuitBreaker => "circuit-breaker",
        }
    }

    /// Parse the strategy string, applying Go's normalization rules:
    /// empty → Adaptive, `"weighted"` → Failover (deprecated strategy; Go
    /// `normalizeRetryPolicy` rewrites it), unknown → Adaptive.
    /// Mirrors `normalizeRetryPolicy` + the `deriveLoadBalancerStrategy` switch.
    pub fn parse(raw: &str) -> Self {
        match raw {
            "failover" | "weighted" => Self::Failover,
            "circuit-breaker" => Self::CircuitBreaker,
            // "adaptive" and any unknown (including empty) fall back to adaptive,
            // matching Go's `default:` arm.
            _ => Self::Adaptive,
        }
    }
}

/// The three enterprise load-balancer implementations selected at request
/// time. Go's orchestrator owns the same three instances and chooses one after
/// deriving the API-key profile override. Keeping the scorers behind
/// [`ScoringStrategy`] preserves the existing strategy/provider extension
/// points while avoiding a second routing implementation in the HTTP layer.
#[derive(Clone)]
pub struct ScoringStrategySet {
    adaptive: Arc<dyn ScoringStrategy>,
    failover: Arc<dyn ScoringStrategy>,
    circuit_breaker: Arc<dyn ScoringStrategy>,
}

impl std::fmt::Debug for ScoringStrategySet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScoringStrategySet")
            .field("adaptive", &self.adaptive.name())
            .field("failover", &self.failover.name())
            .field("circuit_breaker", &self.circuit_breaker.name())
            .finish()
    }
}

impl ScoringStrategySet {
    pub fn new(
        adaptive: Arc<dyn ScoringStrategy>,
        failover: Arc<dyn ScoringStrategy>,
        circuit_breaker: Arc<dyn ScoringStrategy>,
    ) -> Self {
        Self {
            adaptive,
            failover,
            circuit_breaker,
        }
    }

    /// Backward-compatible constructor for callers/tests that supplied one
    /// scorer before per-request strategy selection was wired.
    pub fn uniform(strategy: Arc<dyn ScoringStrategy>) -> Self {
        Self::new(strategy.clone(), strategy.clone(), strategy)
    }

    pub fn get(&self, strategy: LoadBalancerStrategy) -> &dyn ScoringStrategy {
        match strategy {
            LoadBalancerStrategy::Adaptive => self.adaptive.as_ref(),
            LoadBalancerStrategy::Failover => self.failover.as_ref(),
            LoadBalancerStrategy::CircuitBreaker => self.circuit_breaker.as_ref(),
        }
    }

    pub fn get_arc(&self, strategy: LoadBalancerStrategy) -> Arc<dyn ScoringStrategy> {
        match strategy {
            LoadBalancerStrategy::Adaptive => Arc::clone(&self.adaptive),
            LoadBalancerStrategy::Failover => Arc::clone(&self.failover),
            LoadBalancerStrategy::CircuitBreaker => Arc::clone(&self.circuit_breaker),
        }
    }
}

/// Retry policy. Ported from Go `biz.RetryPolicy` (the fields the LB reads;
/// stream/timeout/auto-disable fields belong to other modules).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    pub enabled: bool,
    /// Max number of **different** channels to try after the first (Go
    /// `MaxChannelRetries`).
    pub max_channel_retries: u32,
    /// Max retries on the **same** channel before moving on (Go
    /// `MaxSingleChannelRetries`).
    pub max_single_channel_retries: u32,
    /// Delay between retries in milliseconds (Go `RetryDelayMs`).
    pub retry_delay_ms: u64,
    pub strategy: LoadBalancerStrategy,
}

impl RetryPolicy {
    /// The Go default (`defaultRetryPolicy`): enabled, 3 channel retries, 2
    /// single-channel retries, 1000ms delay, adaptive.
    pub const DEFAULT: Self = Self {
        enabled: true,
        max_channel_retries: 3,
        max_single_channel_retries: 2,
        retry_delay_ms: 1000,
        strategy: LoadBalancerStrategy::Adaptive,
    };

    /// How many candidates the LB must keep for failover: 1 (initial) +
    /// `max_channel_retries`. Mirrors Go `calculateTopK`.
    pub const fn top_k(self) -> usize {
        if self.enabled {
            1 + self.max_channel_retries as usize
        } else {
            1
        }
    }

    /// Total worst-case attempts = channels × (1 + same-channel retries).
    pub const fn max_total_attempts(self) -> u32 {
        if !self.enabled {
            return 1;
        }
        self.top_k() as u32 * (1 + self.max_single_channel_retries)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

// ===========================================================================
// Failover state machine (S06) — port of outbound.NextChannel / PrepareForRetry
// ===========================================================================

/// Failover cursor over an ordered candidate list. Mirrors the subset of Go
/// `PersistenceState` the retry loop mutates: `CurrentCandidateIndex`,
/// `CurrentModelIndex`, plus a per-channel same-channel-retry counter.
///
/// Advancement contract (Go `outbound.go`):
/// - [`FailoverState::prepare_for_retry`] re-attempts the current channel,
///   advancing to its next model when available. Counts toward
///   `max_single_channel_retries`. Returns `false` when the same-channel budget
///   is exhausted.
/// - [`FailoverState::next_channel`] moves to the next candidate (resetting the
///   same-channel counter). Returns `false` when there are no more candidates.
#[derive(Clone, Debug)]
pub struct FailoverState<'a> {
    candidates: &'a [&'a Candidate],
    /// Index into `candidates` of the channel currently being attempted.
    pub current_index: usize,
    /// Index into the current candidate's model list (mirrors Go
    /// `CurrentModelIndex`). The candidate itself owns its model list; this
    /// cursor is advanced by `prepare_for_retry` and is read by the caller to
    /// pick the model. Kept here so the state machine is self-contained.
    pub current_model_index: usize,
    /// Same-channel retries consumed for the current candidate.
    same_channel_retries: u32,
    /// Total attempts made (initial + retries), for observability/caps.
    total_attempts: u32,
}

/// Error returned by [`FailoverState`] when no further attempt is possible.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FailoverError {
    #[error("no more candidates available for retry")]
    NoMoreChannels,
    #[error("single-channel retry budget exhausted for channel {channel_id}")]
    SingleChannelExhausted { channel_id: String },
    #[error("retry policy disabled")]
    RetryDisabled,
}

impl<'a> FailoverState<'a> {
    pub fn new(candidates: &'a [&'a Candidate]) -> Result<Self, FailoverError> {
        if candidates.is_empty() {
            return Err(FailoverError::NoMoreChannels);
        }
        Ok(Self {
            candidates,
            current_index: 0,
            current_model_index: 0,
            same_channel_retries: 0,
            total_attempts: 1,
        })
    }

    pub fn current(&self) -> &'a Candidate {
        self.candidates[self.current_index]
    }

    pub const fn same_channel_retries(&self) -> u32 {
        self.same_channel_retries
    }

    pub const fn total_attempts(&self) -> u32 {
        self.total_attempts
    }

    /// Re-attempt the current channel. Mirrors Go `PrepareForRetry`:
    /// advances `current_model_index` when more models are available (Go checks
    /// `CurrentModelIndex+1 < len(Models)`), otherwise re-runs the last model.
    /// Returns `Ok(true)` if the retry is permitted, `Ok(false)` if the budget
    /// is exhausted (caller should call [`Self::next_channel`]).
    pub fn prepare_for_retry(
        &mut self,
        policy: RetryPolicy,
        model_count: usize,
    ) -> Result<bool, FailoverError> {
        if !policy.enabled {
            return Err(FailoverError::RetryDisabled);
        }
        if self.same_channel_retries >= policy.max_single_channel_retries {
            return Ok(false);
        }
        // Advance to the next model if one is available (Go `PrepareForRetry`
        // first branch); otherwise stay on the last model (Go second branch).
        if self.current_model_index + 1 < model_count {
            self.current_model_index += 1;
        }
        self.same_channel_retries += 1;
        self.total_attempts += 1;
        Ok(true)
    }

    /// Move to the next candidate channel. Mirrors Go `NextChannel`:
    /// increments `CurrentCandidateIndex`, resets `CurrentModelIndex` and the
    /// same-channel retry counter. Returns `Err(NoMoreChannels)` when the list
    /// is exhausted.
    pub fn next_channel(&mut self) -> Result<(), FailoverError> {
        self.current_index += 1;
        if self.current_index >= self.candidates.len() {
            // Roll back so `current()` stays valid if the caller inspects it.
            self.current_index -= 1;
            return Err(FailoverError::NoMoreChannels);
        }
        self.current_model_index = 0;
        self.same_channel_retries = 0;
        self.total_attempts += 1;
        Ok(())
    }
}

// ===========================================================================
// Sticky key (S04) — trace/thread channel reuse
// ===========================================================================

/// Resolves the sticky channel id for a request's trace/thread key. Mirrors Go
/// `TraceAwareStrategy` (which reads from a `ChannelTraceProvider`). The
/// provider is a trait so the LB stays pure-logic; a static provider covers the
/// single-key case and the test fixtures.
///
/// `Send + Sync` so `Arc<dyn StickyKeyProvider>` held by `CommandOrchestrator`
/// keeps the orchestrator `Sync` (required to wire it into async handlers).
pub trait StickyKeyProvider: Send + Sync {
    fn sticky_channel(&self, trace_id: Option<&str>, thread_id: Option<&str>) -> Option<String>;
}

/// A single fixed sticky channel. Mirrors the common case where the caller has
/// already resolved the trace's last-successful channel.
#[derive(Clone, Debug)]
pub struct StaticStickyKeyProvider {
    channel_id: Option<String>,
}

impl StaticStickyKeyProvider {
    pub fn none() -> Self {
        Self { channel_id: None }
    }

    pub fn fixed(channel_id: impl Into<String>) -> Self {
        Self {
            channel_id: Some(channel_id.into()),
        }
    }
}

impl StickyKeyProvider for StaticStickyKeyProvider {
    fn sticky_channel(&self, _trace_id: Option<&str>, _thread_id: Option<&str>) -> Option<String> {
        self.channel_id.clone()
    }
}

/// Reorder a candidate slice so the sticky channel (if present among the
/// candidates) comes first, preserving the relative order of the rest. Mirrors
/// `FilteringCandidateSelector.select_with_context`'s `rotate_left` semantics
/// and Go `TraceAwareStrategy`'s boost-the-last-successful-channel effect, but
/// as a deterministic pre-sort rotation (useful when no scoring strategy runs).
pub fn order_with_sticky<'a>(
    candidates: &'a [&'a Candidate],
    provider: &dyn StickyKeyProvider,
    trace_id: Option<&str>,
    thread_id: Option<&str>,
) -> Vec<&'a Candidate> {
    let Some(sticky_id) = provider.sticky_channel(trace_id, thread_id) else {
        return candidates.to_vec();
    };
    let mut result = candidates.to_vec();
    if let Some(pos) = result.iter().position(|c| c.id == sticky_id) {
        result.rotate_left(pos);
    }
    result
}

// ===========================================================================
// Sub-strategy scorings (S05–S08) — pure-logic adapters over injectable
// provider snapshots. The Go strategies read live metrics/trackers; here we
// keep the LB pure-logic by taking the resolved per-candidate signal as input.
// Each adapter mirrors the Go formula 1:1 (see the per-strategy Go file noted
// in its doc-comment) so the composite weight formula is preserved exactly.
//
// NOTE on altitude: the live backing services (ChannelMetricsProvider,
// ChannelRequestTracker, ModelCircuitBreaker, ProviderQuotaStatusProvider)
// live in other crates that are not yet wired into this pure-logic module.
// The provider traits below are intentionally minimal so a future wiring pass
// can implement them against the real services without touching the composite
// definitions or the selection-count contract.
// ===========================================================================

/// Per-candidate error-aware signal (mirrors Go `biz.AggregatedMetrics` fields
/// read by `ErrorAwareStrategy`). `cooldown_ratio` is the precomputed time
/// decay 0..=1 (Go: `1 - time.Since(lastFailure)/cooldown`), clamped to 0 when
/// outside the cooldown window. `consecutive_failures` mirrors the metric.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ErrorAwareSnapshot {
    pub consecutive_failures: i64,
    pub cooldown_ratio: i64,
}

/// Provider of error-aware signals per candidate id. Mirrors Go
/// `ChannelMetricsProvider.GetChannelMetrics` (only the fields
/// `ErrorAwareStrategy.Score` actually reads).
pub trait ErrorAwareProvider: Send + Sync {
    fn snapshot(&self, candidate_id: &str) -> ErrorAwareSnapshot;
}

/// `ErrorAwareStrategy.Score` (port of `lb_strategy_bp.go`).
///
/// Go formula (per `Score`):
/// ```text
/// score = maxScore                                                  // 200
/// if lastFailure in cooldown:
///     cooldownRatio = 1 - time.Since(lastFailure)/cooldownMinutes
/// penalty = consecutiveFailures * 30 * cooldownRatio
/// score -= penalty
/// score -= 40 * cooldownRatio                                       // base
/// if score < 0: score = 0
/// ```
///
/// To stay in the LB's `i64` score domain while preserving the exact Go
/// formula, the snapshot carries a precomputed `cooldown_ratio` expressed in
/// basis points (0..=10_000, where 10_000 == 1.0). The penalty is then
/// `(consecutive * 30 * bp) / 10_000`, matching Go's f64 result for every
/// value the Go test-suite exercises.
#[derive(Clone, Copy, Debug, Default)]
pub struct ErrorAwareScoring<P: ErrorAwareProvider> {
    provider: P,
    max_score: i64,
    base_penalty_bps: i64, // 40 expressed as the constant penalty.
    per_failure_bps: i64,  // 30
}

impl<P: ErrorAwareProvider> ErrorAwareScoring<P> {
    /// Mirrors Go's defaults: maxScore=200, basePenalty=40, perFailure=30.
    pub const fn new(provider: P) -> Self {
        Self {
            provider,
            max_score: 200,
            base_penalty_bps: 40,
            per_failure_bps: 30,
        }
    }
}

const BPS_DENOM: i64 = 10_000;

impl<P: ErrorAwareProvider> ScoringStrategy for ErrorAwareScoring<P> {
    fn name(&self) -> &'static str {
        "ErrorAware"
    }

    fn score(&self, candidate: &Candidate) -> i64 {
        let snap = self.provider.snapshot(&candidate.id);
        // Neutral fallback when the provider has no data mirrors Go's
        // "if err != nil { return maxScore/2 }".
        let mut score = self.max_score;
        if snap.cooldown_ratio > 0 {
            let failure_penalty =
                snap.consecutive_failures * self.per_failure_bps * snap.cooldown_ratio / BPS_DENOM;
            score -= failure_penalty;
            score -= self.base_penalty_bps * snap.cooldown_ratio / BPS_DENOM;
        }
        if score < 0 {
            score = 0;
        }
        score
    }
}

/// Per-candidate latency-aware signal (mirrors the subset of
/// `biz.AggregatedMetrics` that `LatencyAwareStrategy.Score` consumes).
/// `score` is the precomputed latency score (Go range 0..=80). When `has_signal`
/// is false the strategy returns the neutral `maxScore/2`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LatencyAwareSnapshot {
    pub score: i64,
    pub has_signal: bool,
}

pub trait LatencyAwareProvider: Send + Sync {
    fn snapshot(&self, candidate_id: &str) -> LatencyAwareSnapshot;
}

/// `LatencyAwareStrategy.Score` (port of `lb_strategy_latency.go`). The Go
/// formula (streaming vs non-streaming EWMA math) lives in the provider; the
/// strategy itself is a thin wrapper that falls back to `maxScore/2` when no
/// signal is present. We mirror that fallback exactly so the composite weight
/// is preserved.
#[derive(Clone, Copy, Debug, Default)]
pub struct LatencyAwareScoring<P: LatencyAwareProvider> {
    provider: P,
    max_score: i64,
}

impl<P: LatencyAwareProvider> LatencyAwareScoring<P> {
    /// Mirrors Go's default: maxScore=80 (`defaultLatencyMaxScore`).
    pub const fn new(provider: P) -> Self {
        Self {
            provider,
            max_score: 80,
        }
    }
}

impl<P: LatencyAwareProvider> ScoringStrategy for LatencyAwareScoring<P> {
    fn name(&self) -> &'static str {
        "LatencyAware"
    }

    fn score(&self, candidate: &Candidate) -> i64 {
        let snap = self.provider.snapshot(&candidate.id);
        if snap.has_signal {
            snap.score
        } else {
            // Go: "if !hasSignal { return maxScore / 2 }".
            self.max_score / 2
        }
    }
}

/// Per-candidate weighted-round-robin signal (mirrors what
/// `WeightRoundRobinStrategy.Score` returns after reading metrics +
/// applying the inactivity decay). The score range is 10..=150.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WeightedRoundRobinSnapshot {
    pub score: i64,
}

pub trait WeightedRoundRobinProvider: Send + Sync {
    fn snapshot(&self, candidate_id: &str) -> WeightedRoundRobinSnapshot;
}

/// `WeightRoundRobinStrategy.Score` (port of `lb_strategy_rr.go`). The Go
/// `calculateScore` math (effective-count + exp decay) lives in the provider;
/// this adapter surfaces the resolved score as-is, matching Go's
/// `if err != nil { return (max+min)/2 }` fallback when the provider reports
/// no data (signalled by a `score == 0` snapshot — the strategy never returns
/// 0 in the happy path because `minScore == 10`).
#[derive(Clone, Copy, Debug, Default)]
pub struct WeightedRoundRobinScoring<P: WeightedRoundRobinProvider> {
    provider: P,
    moderate_score: i64, // (maxScore+minScore)/2 = (150+10)/2 = 80
}

impl<P: WeightedRoundRobinProvider> WeightedRoundRobinScoring<P> {
    pub const fn new(provider: P) -> Self {
        Self {
            provider,
            moderate_score: 80,
        }
    }
}

impl<P: WeightedRoundRobinProvider> ScoringStrategy for WeightedRoundRobinScoring<P> {
    fn name(&self) -> &'static str {
        "WeightRoundRobin"
    }

    fn score(&self, candidate: &Candidate) -> i64 {
        let snap = self.provider.snapshot(&candidate.id);
        if snap.score > 0 {
            snap.score
        } else {
            // Go: "if err != nil { return (max+min)/2 }". A snapshot of 0 is
            // impossible in Go's happy path (minScore=10 floor) so we treat it
            // as the "no metrics" branch.
            self.moderate_score
        }
    }
}

/// `RandomStrategy.Score` (port of `lb_strategy_random.go`). Returns a random
/// value in `[min, max]` (Go default 0..=0.5) to break ties. The Rust port
/// takes the value as a parameter so tests stay deterministic; production
/// callers pass `rand::thread_rng().gen_range(0..=50)` (basis-point form).
#[derive(Clone, Copy, Debug, Default)]
pub struct RandomScoring {
    value_bps: i64,
}

impl RandomScoring {
    pub const fn fixed(value_bps: i64) -> Self {
        Self { value_bps }
    }
}

impl ScoringStrategy for RandomScoring {
    fn name(&self) -> &'static str {
        "Random"
    }

    fn score(&self, _candidate: &Candidate) -> i64 {
        self.value_bps
    }
}

/// Per-candidate model-aware circuit-breaker signal (mirrors what
/// `ModelAwareCircuitBreakerStrategy.Score` returns after consulting the
/// model-CB provider: `effectiveWeight * maxScore + rand*0.5`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CircuitBreakerSnapshot {
    /// Fully computed score (Go: `effectiveWeight * 200 + rand*0.5`).
    pub score: i64,
}

pub trait CircuitBreakerProvider: Send + Sync {
    fn snapshot(&self, candidate_id: &str, model: &str) -> CircuitBreakerSnapshot;
}

/// `ModelAwareCircuitBreakerStrategy.Score` (port of
/// `lb_strategy_model_aware_circuit_breaker.go`). The half-open/closed/open
/// state machine lives in the provider; this adapter surfaces the resolved
/// score, matching Go's `maxScore * 0.5` neutral fallback when no model is
/// requested (signalled by an empty model string).
#[derive(Clone, Copy, Debug, Default)]
pub struct ModelAwareCircuitBreakerScoring<P: CircuitBreakerProvider> {
    provider: P,
    model: &'static str,
    max_score: i64,
}

impl<P: CircuitBreakerProvider> ModelAwareCircuitBreakerScoring<P> {
    /// `model` is the requested model id (Go: taken from context). Pass `""`
    /// to trigger the "no model specified" neutral branch.
    pub const fn new(provider: P, model: &'static str) -> Self {
        Self {
            provider,
            model,
            max_score: 200,
        }
    }
}

impl<P: CircuitBreakerProvider> ScoringStrategy for ModelAwareCircuitBreakerScoring<P> {
    fn name(&self) -> &'static str {
        "ModelAwareCircuitBreaker"
    }

    fn score(&self, candidate: &Candidate) -> i64 {
        if self.model.is_empty() {
            // Go: "if modelID == "" { return maxScore * 0.5 }".
            self.max_score / 2
        } else {
            self.provider.snapshot(&candidate.id, self.model).score
        }
    }
}

/// Per-candidate rate-limit-aware signal (mirrors what
/// `RateLimitAwareStrategy.Score` returns after consulting the
/// request-tracker + limiter manager). The score range is 0..=100, plus the
/// `-10_000` exhausted penalty (Go: `rateLimitExhaustedScore`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RateLimitSnapshot {
    pub score: i64,
}

pub trait RateLimitProvider: Send + Sync {
    fn snapshot(&self, candidate_id: &str) -> RateLimitSnapshot;
}

/// `RateLimitAwareStrategy.Score` (port of `lb_strategy_rate_limit.go`). The
/// RPM/TPM/concurrency scoring lives in the provider; this adapter surfaces
/// the resolved score as-is.
#[derive(Clone, Copy, Debug, Default)]
pub struct RateLimitScoring<P: RateLimitProvider> {
    provider: P,
}

impl<P: RateLimitProvider> RateLimitScoring<P> {
    pub const fn new(provider: P) -> Self {
        Self { provider }
    }
}

impl<P: RateLimitProvider> ScoringStrategy for RateLimitScoring<P> {
    fn name(&self) -> &'static str {
        "RateLimitAware"
    }

    fn score(&self, candidate: &Candidate) -> i64 {
        self.provider.snapshot(&candidate.id).score
    }
}

/// Per-candidate quota-aware signal (mirrors what `QuotaAwareStrategy.Score`
/// returns after consulting the quota-status provider + enforcement mode).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QuotaSnapshot {
    pub score: i64,
}

pub trait QuotaProvider: Send + Sync {
    fn snapshot(&self, candidate_id: &str) -> QuotaSnapshot;
}

/// `QuotaAwareStrategy.Score` (port of `lb_strategy_quota.go`). The status
/// branch (Unknown/Exhausted/Warning/Available) lives in the provider; this
/// adapter surfaces the resolved score as-is.
#[derive(Clone, Copy, Debug, Default)]
pub struct QuotaScoring<P: QuotaProvider> {
    provider: P,
}

impl<P: QuotaProvider> QuotaScoring<P> {
    pub const fn new(provider: P) -> Self {
        Self { provider }
    }
}

impl<P: QuotaProvider> ScoringStrategy for QuotaScoring<P> {
    fn name(&self) -> &'static str {
        "QuotaAware"
    }

    fn score(&self, candidate: &Candidate) -> i64 {
        self.provider.snapshot(&candidate.id).score
    }
}

// ===========================================================================
// Composite strategy factories (S05–S08) — mirror the three LoadBalancer
// instances constructed in Go `orchestrator.go` lines 52-65.
//
// All three composites use **equal-weight summation** (each sub-strategy
// contributes `score * 1.0`). This is exactly what Go's `LoadBalancer.
// sortProduction` does: `totalScore += strategy.Score(ctx, channel)` for each
// registered strategy. (Go's `CompositeStrategy` supports custom weights via
// `WithWeights`, but the three production load balancers below are *not*
// constructed via `CompositeStrategy`; they pass strategies directly to
// `NewLoadBalancer`, which sums with weight 1.0.)
// ===========================================================================

/// Build the **adaptive** composite strategy (S05/S06).
///
/// Mirrors Go `orchestrator.go:52-59`:
/// ```text
/// adaptiveLoadBalancer = NewLoadBalancer(systemService, channelService,
///     NewTraceAwareStrategy(requestService),
///     NewErrorAwareStrategy(channelService),
///     NewWeightRoundRobinStrategy(channelService),
///     NewLatencyAwareStrategy(channelService),
///     rateLimitStrategy,
///     quotaStrategy)
/// ```
///
/// Composite formula (sum, all weights == 1.0):
/// ```text
/// adaptive_score(c) = trace_aware(c)        // {0, 1000}: +1000 if c is the
///                                            // trace's last-successful channel
///                  + error_aware(c)         // [0, 200]: 200 - 30*consec*
///                                            //           cooldown - 40*cooldown
///                  + weighted_round_robin(c)// [10, 150]: exp decay on
///                                            //            weight-normalized req
///                                            //            count
///                  + latency_aware(c)       // [0, 80]:   80 - latency penalty,
///                                            //            or 40 (neutral)
///                  + rate_limit_aware(c)    // [-10_000, 100]: exhausted => -10k
///                  + quota_aware(c)         // [-10_000, 0]: exhausted => -10k
/// ```
/// Rate-limit / quota penalties dominate any positive sum so exhausted
/// channels always sort last while remaining available as fallback.
pub fn adaptive_composite(
    trace: TraceAwareScoring,
    error: ErrorAwareScoring<impl ErrorAwareProvider + 'static>,
    weighted_rr: WeightedRoundRobinScoring<impl WeightedRoundRobinProvider + 'static>,
    latency: LatencyAwareScoring<impl LatencyAwareProvider + 'static>,
    rate_limit: RateLimitScoring<impl RateLimitProvider + 'static>,
    quota: QuotaScoring<impl QuotaProvider + 'static>,
) -> CompositeScoring {
    CompositeScoring::new()
        .with(trace)
        .with(error)
        .with(weighted_rr)
        .with(latency)
        .with(rate_limit)
        .with(quota)
}

/// Build the **failover** composite strategy (S07).
///
/// Mirrors Go `orchestrator.go:61-62`:
/// ```text
/// failoverLoadBalancer = NewLoadBalancer(systemService, channelService,
///     NewWeightStrategy(), NewRandomStrategy(), rateLimitStrategy, quotaStrategy)
/// ```
///
/// Composite formula (sum, all weights == 1.0):
/// ```text
/// failover_score(c) = weight(c)        // [0, 100]: ordering_weight/100 * 100
///                   + random(c)        // [0, 0.5]: tie-breaker jitter only
///                   + rate_limit_aware(c) // [-10_000, 100]
///                   + quota_aware(c)   // [-10_000, 0]
/// ```
/// Weight dominates; Random contributes <1 so it cannot reorder distinct
/// weights, only break exact ties (its sole purpose per the Go doc-comment).
pub fn failover_composite(
    weight: WeightScoring,
    random: RandomScoring,
    rate_limit: RateLimitScoring<impl RateLimitProvider + 'static>,
    quota: QuotaScoring<impl QuotaProvider + 'static>,
) -> CompositeScoring {
    CompositeScoring::new()
        .with(weight)
        .with(random)
        .with(rate_limit)
        .with(quota)
}

/// Build the **circuit-breaker** composite strategy (S08).
///
/// Mirrors Go `orchestrator.go:64-65`:
/// ```text
/// circuitBreakerLoadBalancer = NewLoadBalancer(systemService, channelService,
///     NewWeightStrategy(),
///     NewModelAwareCircuitBreakerStrategy(modelCircuitBreaker),
///     rateLimitStrategy,
///     quotaStrategy)
/// ```
///
/// Composite formula (sum, all weights == 1.0):
/// ```text
/// cb_score(c) = weight(c)                         // [0, 100]
///             + model_aware_circuit_breaker(c, m) // [0, 200]: effectiveWeight
///                                                   // * 200 + rand*0.5; 100
///                                                   // neutral when no model
///             + rate_limit_aware(c)               // [-10_000, 100]
///             + quota_aware(c)                    // [-10_000, 0]
/// ```
/// Because CB maxScore (200) > Weight maxScore (100), the CB state can flip
/// the ranking away from the pure weight order — this is the property the Go
/// `TestCircuitBreakerStrategy_Simulation` golden case asserts.
pub fn circuit_breaker_composite(
    weight: WeightScoring,
    circuit_breaker: ModelAwareCircuitBreakerScoring<impl CircuitBreakerProvider + 'static>,
    rate_limit: RateLimitScoring<impl RateLimitProvider + 'static>,
    quota: QuotaScoring<impl QuotaProvider + 'static>,
) -> CompositeScoring {
    CompositeScoring::new()
        .with(weight)
        .with(circuit_breaker)
        .with(rate_limit)
        .with(quota)
}

// ===========================================================================
// Selection-count tracker (S11) — port of Go `ChannelSelectionTracker`
// interface and the `sortProduction`/`sortWithDebug` increment of the top
// candidate's selection counter.
// ===========================================================================

/// Tracks per-candidate selection counts so concurrent/bursty requests see
/// updated counts and spread across channels. Mirrors Go
/// `ChannelSelectionTracker` (load_balancer.go:24-26); the counter is keyed by
/// candidate id (Go uses channel `int`, but this module's `Candidate.id` is a
/// `String` — the contract is otherwise identical).
pub trait SelectionCountTracker {
    fn increment_selection(&self, candidate_id: &str);
}

/// A purely in-memory `SelectionCountTracker` for tests and the future
/// in-process implementation. Mirrors Go's `mockSelectionTracker` (used in
/// `lb_simulation_*_test.go`). The counts are returned as `i64` to match the
/// Go `map[int]int` signature.
#[derive(Debug, Default)]
pub struct InMemorySelectionCounts {
    counts: Mutex<HashMap<String, i64>>,
}

impl InMemorySelectionCounts {
    pub fn new() -> Self {
        Self {
            counts: Mutex::new(HashMap::new()),
        }
    }

    /// Current selection count for `candidate_id` (0 if never selected).
    /// A poisoned mutex means a prior panic left the map inconsistent; treat
    /// it as "no selections recorded" rather than propagating the panic.
    pub fn count_for(&self, candidate_id: &str) -> i64 {
        self.counts
            .lock()
            .ok()
            .and_then(|map| map.get(candidate_id).copied())
            .unwrap_or(0)
    }
}

impl SelectionCountTracker for InMemorySelectionCounts {
    fn increment_selection(&self, candidate_id: &str) {
        if let Ok(mut map) = self.counts.lock() {
            *map.entry(candidate_id.to_string()).or_insert(0) += 1;
        }
    }
}

/// Sort candidates by descending priority, take the top-`top_k`, and increment
/// the selection-count tracker for the resulting top candidate. Mirrors Go
/// `LoadBalancer.sortProduction` lines 219-226:
/// ```text
/// result := lo.Map(scored[:topK], ...)
/// if len(result) > 0 && result[0] != nil && lb.selectionTracker != nil {
///     lb.selectionTracker.IncrementChannelSelection(result[0].Channel.ID)
/// }
/// ```
/// Returns the ordered slice (same as [`sort_candidates_top_k`]).
///
/// Pass `tracker = None` to skip the increment (mirrors Go's nil-tracker
/// branch, e.g. `lb_simulation_adaptive_test.go` passes `nil`).
pub fn sort_candidates_top_k_with_count<'a>(
    candidates: &'a [Candidate],
    strategy: &dyn ScoringStrategy,
    top_k: usize,
    tracker: Option<&dyn SelectionCountTracker>,
) -> Vec<&'a Candidate> {
    let ordered = sort_candidates_top_k(candidates, strategy, top_k);
    if let (Some(tracker), Some(top)) = (tracker, ordered.first()) {
        // Go increments on result[0] unconditionally — the candidate is always
        // present when the slice is non-empty (no nil entries in the Rust port).
        tracker.increment_selection(&top.id);
    }
    ordered
}

/// Debug-scored variant of [`sort_candidates_top_k_with_count`]. Mirrors Go
/// `LoadBalancer.sortWithDebug` (which also increments the tracker at lines
/// 300-304 — the production and debug paths increment identically).
pub fn sort_candidates_with_debug_and_count<'a>(
    candidates: &'a [Candidate],
    strategy: &dyn ScoringStrategy,
    top_k: usize,
    tracker: Option<&dyn SelectionCountTracker>,
) -> (Vec<&'a Candidate>, StrategyBreakdown) {
    let (ordered, breakdown) = sort_candidates_with_debug(candidates, strategy, top_k);
    if let (Some(tracker), Some(top)) = (tracker, ordered.first()) {
        tracker.increment_selection(&top.id);
    }
    (ordered, breakdown)
}

// ===========================================================================
// Pure-logic scoring formulas (S05/S06 refinement — TODO RUST-P9-004 "weighted
// round-robin 精确算法" + "latency 滑动窗口").
//
// The Go `LatencyAwareStrategy`, `RoundRobinStrategy`, and
// `WeightRoundRobinStrategy` push the actual EWMA/decay math into the live
// `ChannelMetricsProvider` (see `lb_strategy_latency.go` / `lb_strategy_rr.go`).
// The pure-logic LB adapters above (`LatencyAwareScoring` /
// `WeightedRoundRobinScoring`) consume precomputed snapshots so the LB stays
// unit-testable. To preserve Go parity exactly at the formula level, the
// helpers below mirror the Go math 1:1 (`calculateScore`,
// `calculateScoreComponents`, `calculateStreamingScore`,
// `calculateNonStreamingScore`, `computeRequestLoad`, `clampNormalized`,
// `clampNormalizedInverse`). A future Rust `ChannelMetricsProvider` impl will
// call these to produce the snapshots; tests assert the same golden numbers as
// the Go `*_test.go` cases (`TestRoundRobinStrategy_Score_*`,
// `TestWeightRoundRobinStrategy_Score_*`, `TestLatencyAwareStrategy_Score_*`).
//
// All math uses `f64` to match Go's `float64` exactly; callers round to the
// `i64` score domain at the snapshot boundary.
// ===========================================================================

/// Default maximum score for the round-robin / weighted-round-robin family
/// (Go `maxScore = 150.0`). Mirrors `lb_strategy_rr.go:69`.
pub const RR_MAX_SCORE: f64 = 150.0;
/// Default minimum score for the round-robin / weighted-round-robin family
/// (Go `minScore = 10.0`). Mirrors `lb_strategy_rr.go:71`.
pub const RR_MIN_SCORE: f64 = 10.0;
/// Cap on the request count considered by the round-robin formulas (Go
/// `requestCountCap = 1000`). Mirrors `lb_strategy_rr.go:74`.
pub const RR_REQUEST_COUNT_CAP: i64 = 1000;
/// Exponential scaling factor for the round-robin decay curve (Go
/// `roundRobinScalingFactor = 150.0`). Mirrors `lb_strategy_rr.go:13`.
pub const RR_SCALING_FACTOR: f64 = 150.0;
/// Inactivity decay window for the round-robin family — 5 minutes (Go
/// `defaultRoundRobinInactivityDecay`). Mirrors `lb_strategy_rr.go:14`.
pub const RR_INACTIVITY_DECAY_SECS: f64 = 300.0;

/// Latency-aware defaults (Go `lb_strategy_latency.go:11-21`).
pub const LATENCY_MAX_SCORE: f64 = 80.0;
pub const LATENCY_STREAMING_FIRST_TOKEN_MAX_MS: f64 = 3000.0;
pub const LATENCY_STREAMING_MIN_TPS: f64 = 5.0;
pub const LATENCY_STREAMING_MAX_TPS: f64 = 100.0;
pub const LATENCY_NON_STREAMING_MAX_MS: f64 = 3000.0;
pub const LATENCY_STREAMING_FIRST_TOKEN_WEIGHT: f64 = 0.7;
pub const LATENCY_STREAMING_THROUGHPUT_WEIGHT: f64 = 0.3;

/// Clamp `value` into `[0,1]` as `(value - min) / (max - min)`. Mirrors Go
/// `clampNormalized` (`lb_strategy_latency.go:164-170`). Returns 0 when
/// `max <= min` (Go early-returns 0 in that case).
pub fn clamp_normalized(value: f64, min: f64, max: f64) -> f64 {
    if max <= min {
        return 0.0;
    }
    clamp01((value - min) / (max - min))
}

/// Clamp the inverse `1 - value/max` into `[0,1]`. Mirrors Go
/// `clampNormalizedInverse` (`lb_strategy_latency.go:156-162`). Returns 0 when
/// `max <= 0`.
pub fn clamp_normalized_inverse(value: f64, max: f64) -> f64 {
    if max <= 0.0 {
        return 0.0;
    }
    clamp01(1.0 - (value / max))
}

#[inline]
fn clamp01(value: f64) -> f64 {
    if value < 0.0 {
        0.0
    } else if value > 1.0 {
        1.0
    } else {
        value
    }
}

/// Inputs to the round-robin / weighted-round-robin scoring formula. Mirrors
/// the subset of Go `biz.AggregatedMetrics` that
/// `RoundRobinStrategy.calculateScoreComponents` and
/// `WeightRoundRobinStrategy.calculateScore` consume:
/// - `request_count` — Go `metrics.RequestCount`.
/// - `inactivity_secs` — seconds since `latestActivityAt(metrics)`. Pass `0.0`
///   when no activity timestamp is recorded (Go: `lastActivity == nil` →
///   `inactivitySeconds = 0`, no decay applied).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RoundRobinFormulaInput {
    pub request_count: i64,
    pub inactivity_secs: f64,
}

/// Effective request count after capping + inactivity decay. Mirrors Go
/// `computeRequestLoad` (`lb_strategy_rr.go:37-61`): cap to
/// [`RR_REQUEST_COUNT_CAP`], then multiply by `exp(-inactivity / decay_secs)`.
/// Returns `(capped_count, effective_count, decay_multiplier)`.
///
/// Note: Go returns `(capped, effective, inactivitySeconds)`; we surface the
/// decay multiplier instead because the caller already knows `inactivity_secs`.
/// The effective count is identical to Go's.
pub fn compute_request_load(input: RoundRobinFormulaInput) -> (f64, f64, f64) {
    let capped = if input.request_count > RR_REQUEST_COUNT_CAP {
        RR_REQUEST_COUNT_CAP as f64
    } else {
        input.request_count as f64
    };
    if capped <= 0.0 {
        return (0.0, 0.0, 1.0);
    }
    let decay_multiplier = if input.inactivity_secs > 0.0 && RR_INACTIVITY_DECAY_SECS > 0.0 {
        std::f64::consts::E.powf(-input.inactivity_secs / RR_INACTIVITY_DECAY_SECS)
    } else {
        1.0
    };
    let effective = capped * decay_multiplier;
    (capped, effective, decay_multiplier)
}

/// Round-robin score (Go `RoundRobinStrategy.calculateScoreComponents`,
/// `lb_strategy_rr.go:201-220`). Range: [`RR_MIN_SCORE`, `RR_MAX_SCORE`].
///
/// Formula (mirrors Go exactly):
/// ```text
/// (capped, effective, _) = compute_request_load(input)
/// raw = if effective > 0 { RR_MAX_SCORE * exp(-effective / RR_SCALING_FACTOR) }
///       else             { RR_MAX_SCORE }
/// score = max(raw, RR_MIN_SCORE)
/// ```
pub fn round_robin_score(input: RoundRobinFormulaInput) -> f64 {
    let (_, effective, _) = compute_request_load(input);
    let raw = if effective > 0.0 {
        RR_MAX_SCORE * std::f64::consts::E.powf(-effective / RR_SCALING_FACTOR)
    } else {
        RR_MAX_SCORE
    };
    if raw < RR_MIN_SCORE {
        RR_MIN_SCORE
    } else {
        raw
    }
}

/// Weighted round-robin score (Go `WeightRoundRobinStrategy.calculateScore`,
/// `lb_strategy_rr.go:272-309`). Range: slightly above `RR_MIN_SCORE` up to
/// `RR_MAX_SCORE`.
///
/// `ordering_weight` is Go `channel.OrderingWeight`. A weight of 0 collapses to
/// standard round-robin behavior (Go: `weightFactor = 1.0` when `<= 0`).
///
/// Formula (mirrors Go exactly, including the soft clamp branch):
/// ```text
/// (capped, effective, _) = compute_request_load(input)
/// weight_factor = if ordering_weight > 0 { ordering_weight / 100.0 } else { 1.0 }
/// normalized = effective / weight_factor
/// raw = if normalized > 0 { RR_MAX_SCORE * exp(-normalized / RR_SCALING_FACTOR) }
///       else              { RR_MAX_SCORE }
/// score = if raw < RR_MIN_SCORE { RR_MIN_SCORE + raw / RR_MAX_SCORE } else { raw }
/// ```
pub fn weighted_round_robin_score(input: RoundRobinFormulaInput, ordering_weight: i64) -> f64 {
    let (_, effective, _) = compute_request_load(input);
    let weight_factor = if ordering_weight > 0 {
        ordering_weight as f64 / 100.0
    } else {
        1.0
    };
    let normalized = effective / weight_factor;
    let raw = if normalized > 0.0 {
        RR_MAX_SCORE * std::f64::consts::E.powf(-normalized / RR_SCALING_FACTOR)
    } else {
        RR_MAX_SCORE
    };
    if raw < RR_MIN_SCORE {
        // Go's soft clamp: minScore + (raw / maxScore). Keeps heavily-loaded
        // channels distinguishable just above the floor instead of collapsing
        // them all onto minScore.
        RR_MIN_SCORE + raw / RR_MAX_SCORE
    } else {
        raw
    }
}

/// Latency-aware EWMA snapshot used by the streaming / non-streaming scoring
/// formulas. Mirrors the subset of Go `biz.AggregatedMetrics` read by
/// `LatencyAwareStrategy.calculateStreamingScore` /
/// `calculateNonStreamingScore`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LatencyFormulaInput {
    /// Go `metrics.StreamingFirstTokenLatencyEWMA`.
    pub streaming_first_token_latency_ewma_ms: f64,
    /// Go `metrics.StreamingTokensPerSecondEWMA`.
    pub streaming_tokens_per_second_ewma: f64,
    /// Go `metrics.StreamingSampleCount`.
    pub streaming_sample_count: i64,
    /// Go `metrics.NonStreamingLatencyEWMA`.
    pub non_streaming_latency_ewma_ms: f64,
    /// Go `metrics.NonStreamingSampleCount`.
    pub non_streaming_sample_count: i64,
}

/// Result of a latency-aware score calculation. `has_signal` mirrors Go's
/// `hasSignal` return: when false the strategy returns the neutral
/// `LATENCY_MAX_SCORE / 2`. `score` is the raw Go score (uncapped by the
/// neutral fallback) so callers can apply the fallback themselves.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LatencyScore {
    pub score: f64,
    pub has_signal: bool,
}

/// Streaming latency score (Go `calculateStreamingScore`,
/// `lb_strategy_latency.go:110-135`). Returns `has_signal = false` when
/// `streaming_sample_count == 0`.
///
/// Formula:
/// ```text
/// first_token_score = clamp_normalized_inverse(first_token_ewma, 3000)
/// throughput_score  = if tps_ewma > 0 {
///                         clamp_normalized(tps_ewma, 5, 100)
///                     } else { 0.5 }
/// score = 80 * (0.7 * first_token_score + 0.3 * throughput_score)
/// ```
pub fn streaming_latency_score(input: LatencyFormulaInput) -> LatencyScore {
    if input.streaming_sample_count == 0 {
        return LatencyScore {
            score: 0.0,
            has_signal: false,
        };
    }
    let first_token_score = clamp_normalized_inverse(
        input.streaming_first_token_latency_ewma_ms,
        LATENCY_STREAMING_FIRST_TOKEN_MAX_MS,
    );
    let throughput_score = if input.streaming_tokens_per_second_ewma > 0.0 {
        clamp_normalized(
            input.streaming_tokens_per_second_ewma,
            LATENCY_STREAMING_MIN_TPS,
            LATENCY_STREAMING_MAX_TPS,
        )
    } else {
        0.5
    };
    let score = LATENCY_MAX_SCORE
        * (LATENCY_STREAMING_FIRST_TOKEN_WEIGHT * first_token_score
            + LATENCY_STREAMING_THROUGHPUT_WEIGHT * throughput_score);
    LatencyScore {
        score,
        has_signal: true,
    }
}

/// Non-streaming latency score (Go `calculateNonStreamingScore`,
/// `lb_strategy_latency.go:137-154`). Returns `has_signal = false` when
/// `non_streaming_sample_count == 0`.
///
/// Formula: `score = 80 * clamp_normalized_inverse(latency_ewma, 3000)`.
pub fn non_streaming_latency_score(input: LatencyFormulaInput) -> LatencyScore {
    if input.non_streaming_sample_count == 0 {
        return LatencyScore {
            score: 0.0,
            has_signal: false,
        };
    }
    let latency_component = clamp_normalized_inverse(
        input.non_streaming_latency_ewma_ms,
        LATENCY_NON_STREAMING_MAX_MS,
    );
    LatencyScore {
        score: LATENCY_MAX_SCORE * latency_component,
        has_signal: true,
    }
}

/// Resolve a [`LatencyScore`] to the value the strategy actually returns,
/// applying Go's `if !hasSignal { return maxScore / 2 }` fallback. Mirrors
/// `LatencyAwareStrategy.Score` (`lb_strategy_latency.go:42-54`).
pub fn resolve_latency_score(score: LatencyScore) -> f64 {
    if score.has_signal {
        score.score
    } else {
        LATENCY_MAX_SCORE / 2.0
    }
}

// ===========================================================================
// Retryable judgment (S04) — port of retry.go
// ===========================================================================

/// Whether `status_code` is retryable by default. Mirrors Go
/// `httpclient.IsHTTPStatusCodeRetryable`: 429 (Too Many Requests) is
/// retryable; other 4xx codes are not; all 5xx codes are retryable; success /
/// redirect codes are not.
pub fn is_retryable_status(status_code: i64) -> bool {
    if status_code == 429 {
        return true; // 429 is retryable (rate limiting).
    }
    if (400..500).contains(&status_code) {
        return false; // Other 4xx errors are not retryable.
    }
    // 5xx errors are retryable; everything else (1xx/2xx/3xx) is not.
    (500..600).contains(&status_code)
}

/// Whether `status_code` is retryable for this channel: default set, or the
/// channel's per-channel override list. Mirrors the status-code branch of Go
/// `isRetryableErrorForChannel`.
pub fn is_retryable_status_for_channel(status_code: i64, retryable_status_codes: &[i64]) -> bool {
    is_retryable_status(status_code) || retryable_status_codes.contains(&status_code)
}

/// Whether `message` matches any retryable error pattern. Mirrors Go
/// `matchesRetryableErrorPattern`: regex patterns use `Regex::is_match`,
/// non-regex patterns use case-sensitive substring containment.
pub fn matches_retryable_error_pattern(message: &str, patterns: &[RetryableErrorPattern]) -> bool {
    if message.is_empty() || patterns.is_empty() {
        return false;
    }
    for pattern in patterns {
        if pattern.pattern.is_empty() {
            continue;
        }
        if pattern.regex {
            if Regex::new(&pattern.pattern).is_ok_and(|re| re.is_match(message)) {
                return true;
            }
        } else if message.contains(&pattern.pattern) {
            return true;
        }
    }
    false
}

/// Full retryable judgment for a channel (Go `isRetryableErrorForChannel`):
/// retryable status (default or channel override), or a matching error pattern.
pub fn is_retryable_for_channel(
    status_code: i64,
    error_message: &str,
    retryable_status_codes: &[i64],
    retryable_error_patterns: &[RetryableErrorPattern],
) -> bool {
    is_retryable_status_for_channel(status_code, retryable_status_codes)
        || matches_retryable_error_pattern(error_message, retryable_error_patterns)
}

// ===========================================================================
// High-level facade — the orchestrator-facing entry point (S04–S10)
// ===========================================================================

/// Resolve the load-balancer strategy, applying Go's normalization
/// (`deriveLoadBalancerStrategy` + `normalizeRetryPolicy`): the per-API-key
/// profile override wins unless it is empty / `"system_default"`, and
/// `"weighted"` is rewritten to `Failover`.
pub fn resolve_strategy(
    policy_strategy: &str,
    api_key_profile_strategy: Option<&str>,
) -> LoadBalancerStrategy {
    let raw = match api_key_profile_strategy {
        Some(s) if !s.is_empty() && s != "system_default" => s,
        _ => policy_strategy,
    };
    LoadBalancerStrategy::parse(raw)
}

/// One-shot selection: sort candidates, apply top-k from the retry policy, then
/// bias toward the sticky channel. Returns the ordered list of channels to try
/// (the caller drives [`FailoverState`] over the result). Mirrors Go
/// `LoadBalancer.Sort` end-to-end.
pub fn select_channels<'a>(
    candidates: &'a [Candidate],
    strategy: &dyn ScoringStrategy,
    policy: RetryPolicy,
    sticky: &dyn StickyKeyProvider,
    trace_id: Option<&str>,
    thread_id: Option<&str>,
) -> Vec<&'a Candidate> {
    let top_k = policy.top_k().min(candidates.len());
    let mut result = sort_candidates_top_k(candidates, strategy, candidates.len());
    // Go's TraceAware score is applied before top-k selection. Move the sticky
    // candidate while the full eligible set is still present so a healthy
    // historical channel cannot be truncated before its preference applies.
    if let Some(sticky_id) = sticky.sticky_channel(trace_id, thread_id)
        && let Some(pos) = result.iter().position(|c| c.id == sticky_id)
    {
        result.rotate_left(pos);
    }
    result.truncate(top_k);
    if let Some(selected) = result.first() {
        strategy.record_selection(selected);
    }
    result
}

/// Select channels like [`select_channels`], but distribute the leading group
/// of equal-score/equal-priority candidates using a caller-provided stable
/// offset. This avoids permanently preferring DB input order while preserving
/// strict priority boundaries and retry order. A request/trace hash is a good
/// offset source: it is lock-free, evenly distributed and trace-sticky.
pub fn select_channels_with_tie_rotation<'a>(
    candidates: &'a [Candidate],
    strategy: &dyn ScoringStrategy,
    policy: RetryPolicy,
    sticky: &dyn StickyKeyProvider,
    trace_id: Option<&str>,
    thread_id: Option<&str>,
    offset: usize,
) -> Vec<&'a Candidate> {
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut scored = score_candidates(candidates, strategy);
    scored.sort_by(compare_scored);

    let leading_score = scored[0].score;
    let leading_weight = scored[0].candidate.ordering_weight;
    let tie_len = scored
        .iter()
        .take_while(|entry| {
            entry.score == leading_score && entry.candidate.ordering_weight == leading_weight
        })
        .count();
    if tie_len > 1 {
        scored[..tie_len].rotate_left(offset % tie_len);
    }

    let mut result: Vec<&Candidate> = scored.into_iter().map(|entry| entry.candidate).collect();

    if let Some(sticky_id) = sticky.sticky_channel(trace_id, thread_id)
        && let Some(pos) = result
            .iter()
            .position(|candidate| candidate.id == sticky_id)
    {
        result.rotate_left(pos);
    }
    result.truncate(policy.top_k().min(candidates.len()));
    if let Some(selected) = result.first() {
        strategy.record_selection(selected);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::{Candidate, CandidateStatus};

    fn weighted_candidate(id: &str, weight: u32) -> Candidate {
        Candidate::new(id, "provider-a", "gpt-4o", CandidateStatus::Ready).with_weight(weight)
    }

    #[test]
    fn retry_top_k_is_one_when_retry_is_disabled() {
        let policy = RetryTopKPolicy::new(false, 4);

        assert_eq!(policy.top_k(), 1);
    }

    #[test]
    fn adaptive_weighted_scoring_converges_to_configured_channel_share() {
        let candidates = vec![
            Candidate::new("high", "provider", "model", CandidateStatus::Ready)
                .with_ordering_weight(100),
            Candidate::new("low", "provider", "model", CandidateStatus::Ready)
                .with_ordering_weight(50),
        ];
        let strategy = AdaptiveWeightedScoring::new();
        let sticky = StaticStickyKeyProvider::none();
        let policy = RetryPolicy {
            enabled: false,
            ..RetryPolicy::DEFAULT
        };

        let selected: Vec<String> = (0..6)
            .filter_map(|_| {
                select_channels_with_tie_rotation(
                    &candidates,
                    &strategy,
                    policy,
                    &sticky,
                    None,
                    None,
                    0,
                )
                .first()
                .map(|candidate| candidate.id.clone())
            })
            .collect();

        assert_eq!(selected, ["high", "low", "high", "high", "low", "high"]);
        assert_eq!(strategy.selection_count("high"), 4);
        assert_eq!(strategy.selection_count("low"), 2);
    }

    #[test]
    fn retry_top_k_is_one_when_retry_enabled_with_zero_max_retries() {
        let policy = RetryTopKPolicy::new(true, 0);

        assert_eq!(policy.top_k(), 1);
    }

    #[test]
    fn retry_top_k_includes_initial_attempt_and_channel_retries() {
        let policy = RetryTopKPolicy::new(true, 3);

        assert_eq!(policy.top_k(), 4);
    }

    #[test]
    fn failover_attempt_order_preserves_input_order() {
        let candidates = vec![
            weighted_candidate("first", 1),
            weighted_candidate("second", 1),
            weighted_candidate("third", 1),
        ];

        let attempts: Vec<&str> = failover_attempt_order(&candidates, candidates.len())
            .into_iter()
            .map(|candidate| candidate.id.as_str())
            .collect();

        assert_eq!(attempts, vec!["first", "second", "third"]);
    }

    #[test]
    fn failover_attempt_order_truncates_to_top_k() {
        let candidates = vec![
            weighted_candidate("first", 1),
            weighted_candidate("second", 1),
            weighted_candidate("third", 1),
        ];

        let attempts: Vec<&str> = failover_attempt_order(&candidates, 2)
            .into_iter()
            .map(|candidate| candidate.id.as_str())
            .collect();

        assert_eq!(attempts, vec!["first", "second"]);
    }

    #[test]
    fn failover_attempt_order_does_not_skip_first_candidate() {
        let candidates = vec![
            weighted_candidate("first", 1),
            weighted_candidate("second", 1),
            weighted_candidate("third", 1),
        ];

        let attempts: Vec<&str> = failover_attempt_order(&candidates, 1)
            .into_iter()
            .map(|candidate| candidate.id.as_str())
            .collect();

        assert_eq!(attempts, vec!["first"]);
    }

    #[test]
    fn tie_rotation_distributes_equals_without_crossing_priority_boundary() {
        let candidates = vec![
            candidate_with_ordering("a", 10),
            candidate_with_ordering("b", 10),
            candidate_with_ordering("lower", 1),
        ];
        let policy = RetryPolicy {
            enabled: true,
            max_channel_retries: 2,
            ..RetryPolicy::DEFAULT
        };
        let sticky = StaticStickyKeyProvider::none();

        let first: Vec<&str> = select_channels_with_tie_rotation(
            &candidates,
            &WeightScoring::new(),
            policy,
            &sticky,
            None,
            None,
            0,
        )
        .into_iter()
        .map(|candidate| candidate.id.as_str())
        .collect();
        let second: Vec<&str> = select_channels_with_tie_rotation(
            &candidates,
            &WeightScoring::new(),
            policy,
            &sticky,
            None,
            None,
            1,
        )
        .into_iter()
        .map(|candidate| candidate.id.as_str())
        .collect();

        assert_eq!(first, vec!["a", "b", "lower"]);
        assert_eq!(second, vec!["b", "a", "lower"]);
    }

    #[test]
    fn weighted_round_robin_is_stable_for_fixed_input() {
        let candidates = vec![
            weighted_candidate("heavy", 3),
            weighted_candidate("medium", 2),
            weighted_candidate("light", 1),
        ];
        let mut balancer = WeightedRoundRobin::new();

        let selected: Vec<&str> = (0..6)
            .filter_map(|_| balancer.select(&candidates))
            .map(|candidate| candidate.id.as_str())
            .collect();

        assert_eq!(
            selected,
            vec!["heavy", "medium", "heavy", "light", "medium", "heavy"]
        );
    }

    #[test]
    fn bucketed_round_robin_keeps_model_request_type_state_independent() {
        let candidates = vec![
            weighted_candidate("first", 1),
            weighted_candidate("second", 1),
        ];
        let balancer = BucketedRoundRobin::new();

        let chat_first = balancer
            .select("gpt-4o", RequestType::Chat, &candidates)
            .map(|candidate| candidate.id.as_str());
        let chat_second = balancer
            .select("gpt-4o", RequestType::Chat, &candidates)
            .map(|candidate| candidate.id.as_str());
        let embedding_first = balancer
            .select("gpt-4o", RequestType::Embedding, &candidates)
            .map(|candidate| candidate.id.as_str());
        let other_model_first = balancer
            .select("gpt-4o-mini", RequestType::Chat, &candidates)
            .map(|candidate| candidate.id.as_str());

        assert_eq!(chat_first, Some("first"));
        assert_eq!(chat_second, Some("second"));
        assert_eq!(embedding_first, Some("first"));
        assert_eq!(other_model_first, Some("first"));
    }

    #[test]
    fn zero_weight_candidates_are_never_selected() {
        let candidates = vec![
            weighted_candidate("zero", 0),
            weighted_candidate("active", 1),
        ];
        let mut balancer = WeightedRoundRobin::new();

        let selected: Vec<&str> = (0..3)
            .filter_map(|_| balancer.select(&candidates))
            .map(|candidate| candidate.id.as_str())
            .collect();

        assert_eq!(selected, vec!["active", "active", "active"]);
    }

    #[test]
    fn no_selection_when_all_weights_are_zero() {
        let candidates = vec![
            weighted_candidate("zero-a", 0),
            weighted_candidate("zero-b", 0),
        ];
        let mut balancer = WeightedRoundRobin::new();

        assert_eq!(balancer.select(&candidates), None);
    }

    #[test]
    fn debug_selection_includes_score_breakdown() -> Result<(), Box<dyn std::error::Error>> {
        let candidates = vec![
            weighted_candidate("heavy", 3),
            weighted_candidate("light", 1),
        ];
        let mut balancer = WeightedRoundRobin::new();

        let selection = balancer.select_with_debug(&candidates, true);
        let breakdown = selection
            .breakdown
            .ok_or_else(|| "debug breakdown".to_string())?;

        assert_eq!(
            selection.candidate.map(|candidate| candidate.id.as_str()),
            Some("heavy")
        );
        assert_eq!(breakdown.scores.len(), 2);
        assert_eq!(breakdown.scores[0].strategy, "weighted_round_robin");
        assert_eq!(breakdown.scores[0].candidate_id, "heavy");
        assert_eq!(
            breakdown.scores[0].components,
            vec![
                ScoreComponent {
                    name: "previous_current_weight".to_string(),
                    value: 0,
                },
                ScoreComponent {
                    name: "effective_weight".to_string(),
                    value: 3,
                },
                ScoreComponent {
                    name: "current_weight".to_string(),
                    value: 3,
                },
                ScoreComponent {
                    name: "total_weight".to_string(),
                    value: 4,
                },
            ]
        );
        Ok(())
    }

    #[test]
    fn non_debug_selection_does_not_generate_breakdown() {
        let candidates = vec![weighted_candidate("active", 1)];
        let mut balancer = WeightedRoundRobin::new();

        let selection = balancer.select_with_debug(&candidates, false);

        assert_eq!(
            selection.candidate.map(|candidate| candidate.id.as_str()),
            Some("active")
        );
        assert_eq!(selection.breakdown, None);
    }

    #[test]
    fn ordering_weight_tie_break_prefers_higher_ordering_weight() {
        let lighter = OrderingWeightTieBreak::new(10, 0);
        let heavier = OrderingWeightTieBreak::new(20, 1);

        assert_eq!(
            compare_ordering_weight_tie_break(lighter, heavier),
            Ordering::Less
        );
        assert_eq!(
            compare_ordering_weight_tie_break(heavier, lighter),
            Ordering::Greater
        );
    }

    #[test]
    fn ordering_weight_tie_break_does_not_use_channel_id_bias() {
        let z_channel = OrderingWeightTieBreak::new(10, 0);
        let a_channel = OrderingWeightTieBreak::new(10, 1);

        assert_eq!(
            compare_ordering_weight_tie_break(z_channel, a_channel),
            Ordering::Greater
        );
        assert_eq!(
            compare_ordering_weight_tie_break(a_channel, z_channel),
            Ordering::Less
        );
    }

    // -----------------------------------------------------------------------
    // New tests (RUST-P9-004): sorting, strategy, failover, sticky, retryable
    // -----------------------------------------------------------------------

    fn candidate_with_ordering(id: &str, ordering_weight: i64) -> Candidate {
        Candidate::new(id, "provider-a", "gpt-4o", CandidateStatus::Ready)
            .with_ordering_weight(ordering_weight)
    }

    // A strategy that returns a fixed score per candidate id. Lets tests assert
    // exact ordering without depending on WeightScoring's normalization.
    struct FixedScores(HashMap<String, i64>);
    impl ScoringStrategy for FixedScores {
        fn name(&self) -> &'static str {
            "Fixed"
        }
        fn score(&self, candidate: &Candidate) -> i64 {
            self.0.get(&candidate.id).copied().unwrap_or(0)
        }
    }

    fn fixed(pairs: &[(&str, i64)]) -> FixedScores {
        FixedScores(pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect())
    }

    #[test]
    fn sort_candidates_orders_by_descending_score() {
        let candidates = vec![
            candidate_with_ordering("low", 0),
            candidate_with_ordering("high", 0),
            candidate_with_ordering("mid", 0),
        ];
        let strategy = fixed(&[("low", 10), ("mid", 50), ("high", 90)]);

        let ordered: Vec<&str> = sort_candidates_top_k(&candidates, &strategy, candidates.len())
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();

        assert_eq!(ordered, vec!["high", "mid", "low"]);
    }

    #[test]
    fn sort_candidates_tie_breaks_on_ordering_weight_then_input_order() {
        // Two candidates with equal score; higher ordering_weight wins.
        let candidates = vec![
            candidate_with_ordering("a", 5),
            candidate_with_ordering("b", 10),
        ];
        let strategy = fixed(&[("a", 50), ("b", 50)]);

        let ordered: Vec<&str> = sort_candidates_top_k(&candidates, &strategy, 2)
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();

        assert_eq!(ordered, vec!["b", "a"]);
    }

    #[test]
    fn sort_candidates_preserves_input_order_on_full_tie() {
        // Equal score AND equal ordering_weight → input order preserved (Go's
        // "do NOT use channel ID as tie-breaker" rule).
        let candidates = vec![
            candidate_with_ordering("zeta", 7),
            candidate_with_ordering("alpha", 7),
        ];
        let strategy = fixed(&[("zeta", 50), ("alpha", 50)]);

        let ordered: Vec<&str> = sort_candidates_top_k(&candidates, &strategy, 2)
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();

        assert_eq!(ordered, vec!["zeta", "alpha"]);
    }

    #[test]
    fn sort_candidates_top_k_truncates() {
        let candidates = vec![
            candidate_with_ordering("a", 0),
            candidate_with_ordering("b", 0),
            candidate_with_ordering("c", 0),
        ];
        let strategy = fixed(&[("a", 1), ("b", 2), ("c", 3)]);

        let ordered: Vec<&str> = sort_candidates_top_k(&candidates, &strategy, 2)
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();

        assert_eq!(ordered, vec!["c", "b"]);
    }

    #[test]
    fn sort_candidates_single_candidate_skips_sort() {
        let candidates = vec![candidate_with_ordering("solo", 0)];
        let strategy = fixed(&[("solo", 1)]);

        let ordered: Vec<&str> = sort_candidates_top_k(&candidates, &strategy, 5)
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();

        assert_eq!(ordered, vec!["solo"]);
    }

    #[test]
    fn weight_scoring_normalizes_ordering_weight() {
        let strategy = WeightScoring::new();
        let high = candidate_with_ordering("h", 100);
        let mid = candidate_with_ordering("m", 50);
        let zero = candidate_with_ordering("z", 0);

        assert_eq!(strategy.score(&high), 100);
        assert_eq!(strategy.score(&mid), 50);
        assert_eq!(strategy.score(&zero), 0);
    }

    #[test]
    fn weight_scoring_clamps_negative_ordering_weight() {
        let strategy = WeightScoring::new();
        let neg = candidate_with_ordering("neg", -50);

        assert_eq!(strategy.score(&neg), 0);
    }

    #[test]
    fn trace_aware_scoring_boosts_matching_channel() {
        let strategy = TraceAwareScoring::for_channel("channel-b");
        let matching = candidate_with_ordering("channel-b", 0);
        let other = candidate_with_ordering("channel-a", 0);

        assert_eq!(strategy.score(&matching), TraceAwareScoring::DEFAULT_BOOST);
        assert_eq!(strategy.score(&other), 0);
    }

    #[test]
    fn composite_scoring_sums_member_scores() -> Result<(), Box<dyn std::error::Error>> {
        let candidates = vec![
            candidate_with_ordering("boosted", 50),
            candidate_with_ordering("plain", 90),
        ];
        let composite = CompositeScoring::new()
            .with(TraceAwareScoring::for_channel("boosted"))
            .with(WeightScoring::new());

        // boosted = 1000 (trace) + 50 (weight=50→50) = 1050
        // plain   =    0 (trace) + 90 (weight=90→90) = 90
        let ordered: Vec<&str> = sort_candidates_top_k(&candidates, &composite, 2)
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(ordered, vec!["boosted", "plain"]);

        // Debug breakdown flattens per-strategy components.
        let (_, breakdown) = sort_candidates_with_debug(&candidates, &composite, 2);
        let boosted_row = breakdown
            .scores
            .iter()
            .find(|s| s.candidate_id == "boosted")
            .ok_or("boosted row missing")?;
        assert_eq!(boosted_row.strategy, "composite");
        // Two components: one per member strategy.
        assert_eq!(boosted_row.components.len(), 2);
        Ok(())
    }

    #[test]
    fn load_balancer_strategy_parse_normalizes() {
        assert_eq!(
            LoadBalancerStrategy::parse("adaptive"),
            LoadBalancerStrategy::Adaptive
        );
        assert_eq!(
            LoadBalancerStrategy::parse("failover"),
            LoadBalancerStrategy::Failover
        );
        assert_eq!(
            LoadBalancerStrategy::parse("circuit-breaker"),
            LoadBalancerStrategy::CircuitBreaker
        );
        // Unknown and empty fall back to adaptive.
        assert_eq!(
            LoadBalancerStrategy::parse(""),
            LoadBalancerStrategy::Adaptive
        );
        assert_eq!(
            LoadBalancerStrategy::parse("nonsense"),
            LoadBalancerStrategy::Adaptive
        );
    }

    #[test]
    fn resolve_strategy_prefers_api_key_profile_unless_default() {
        // Profile override wins.
        assert_eq!(
            resolve_strategy("adaptive", Some("failover")),
            LoadBalancerStrategy::Failover
        );
        // Empty / "system_default" / None fall back to the policy strategy.
        assert_eq!(
            resolve_strategy("circuit-breaker", Some("")),
            LoadBalancerStrategy::CircuitBreaker
        );
        assert_eq!(
            resolve_strategy("circuit-breaker", Some("system_default")),
            LoadBalancerStrategy::CircuitBreaker
        );
        assert_eq!(
            resolve_strategy("failover", None),
            LoadBalancerStrategy::Failover
        );
    }

    #[test]
    fn retry_policy_default_matches_go() {
        let p = RetryPolicy::DEFAULT;
        assert!(p.enabled);
        assert_eq!(p.max_channel_retries, 3);
        assert_eq!(p.max_single_channel_retries, 2);
        assert_eq!(p.retry_delay_ms, 1000);
        assert_eq!(p.strategy, LoadBalancerStrategy::Adaptive);
        assert_eq!(p.top_k(), 4); // 1 + 3 channel retries
    }

    #[test]
    fn retry_policy_top_k_is_one_when_disabled() {
        let p = RetryPolicy {
            enabled: false,
            max_channel_retries: 5,
            max_single_channel_retries: 2,
            retry_delay_ms: 0,
            strategy: LoadBalancerStrategy::Adaptive,
        };
        assert_eq!(p.top_k(), 1);
        assert_eq!(p.max_total_attempts(), 1);
    }

    #[test]
    fn retry_policy_max_total_attempts_accounts_for_same_channel_retries() {
        let p = RetryPolicy {
            enabled: true,
            max_channel_retries: 2,
            max_single_channel_retries: 1,
            retry_delay_ms: 0,
            strategy: LoadBalancerStrategy::Adaptive,
        };
        // top_k = 3 channels, each with 1+1 = 2 attempts → 6 total.
        assert_eq!(p.max_total_attempts(), 6);
    }

    #[test]
    fn failover_state_starts_at_first_candidate() -> Result<(), FailoverError> {
        let a = candidate_with_ordering("a", 0);
        let b = candidate_with_ordering("b", 0);
        let candidates: Vec<&Candidate> = vec![&a, &b];
        let state = FailoverState::new(&candidates)?;

        assert_eq!(state.current().id, "a");
        assert_eq!(state.current_index, 0);
        assert_eq!(state.same_channel_retries(), 0);
        assert_eq!(state.total_attempts(), 1);
        Ok(())
    }

    #[test]
    fn failover_state_empty_candidate_list_is_an_error() {
        let empty: Vec<&Candidate> = vec![];
        match FailoverState::new(&empty) {
            Err(FailoverError::NoMoreChannels) => {}
            other => panic!("expected NoMoreChannels, got {other:?}"),
        }
    }

    #[test]
    fn prepare_for_retry_advances_model_then_repeats_last() -> Result<(), FailoverError> {
        let a = candidate_with_ordering("a", 0);
        let candidates: Vec<&Candidate> = vec![&a, &a];
        let mut state = FailoverState::new(&candidates)?;
        let policy = RetryPolicy {
            enabled: true,
            max_channel_retries: 3,
            max_single_channel_retries: 2,
            retry_delay_ms: 0,
            strategy: LoadBalancerStrategy::Adaptive,
        };

        // Channel "a" has 3 models available.
        assert!(state.prepare_for_retry(policy, 3)?);
        assert_eq!(state.current_model_index, 1, "should advance to model 1");
        assert_eq!(state.same_channel_retries(), 1);

        assert!(state.prepare_for_retry(policy, 3)?);
        assert_eq!(state.current_model_index, 2, "should advance to model 2");
        assert_eq!(state.same_channel_retries(), 2);

        // Budget exhausted.
        assert!(!state.prepare_for_retry(policy, 3)?);
        assert_eq!(
            state.same_channel_retries(),
            2,
            "counter not incremented past cap"
        );
        Ok(())
    }

    #[test]
    fn prepare_for_retry_repeats_last_model_when_no_more_models() -> Result<(), FailoverError> {
        let a = candidate_with_ordering("a", 0);
        let candidates: Vec<&Candidate> = vec![&a];
        let mut state = FailoverState::new(&candidates)?;
        let policy = RetryPolicy {
            enabled: true,
            max_channel_retries: 3,
            max_single_channel_retries: 1,
            retry_delay_ms: 0,
            strategy: LoadBalancerStrategy::Adaptive,
        };

        // Only 1 model: stays on index 0 but still consumes the retry budget.
        assert!(state.prepare_for_retry(policy, 1)?);
        assert_eq!(state.current_model_index, 0);
        assert_eq!(state.same_channel_retries(), 1);
        assert!(!state.prepare_for_retry(policy, 1)?);
        Ok(())
    }

    #[test]
    fn prepare_for_retry_errors_when_policy_disabled() -> Result<(), FailoverError> {
        let a = candidate_with_ordering("a", 0);
        let candidates: Vec<&Candidate> = vec![&a];
        let mut state = FailoverState::new(&candidates)?;
        let policy = RetryPolicy {
            enabled: false,
            max_channel_retries: 0,
            max_single_channel_retries: 0,
            retry_delay_ms: 0,
            strategy: LoadBalancerStrategy::Adaptive,
        };

        match state.prepare_for_retry(policy, 1) {
            Err(FailoverError::RetryDisabled) => {}
            Err(other) => {
                return Err(other);
            }
            Ok(value) => panic!("expected RetryDisabled, got Ok({value})"),
        }
        Ok(())
    }

    #[test]
    fn next_channel_advances_and_resets_same_channel_counter() -> Result<(), FailoverError> {
        let a = candidate_with_ordering("a", 0);
        let b = candidate_with_ordering("b", 0);
        let candidates: Vec<&Candidate> = vec![&a, &b];
        let mut state = FailoverState::new(&candidates)?;
        let policy = RetryPolicy::DEFAULT;

        // Burn one same-channel retry on "a".
        state.prepare_for_retry(policy, 1)?;
        assert_eq!(state.same_channel_retries(), 1);

        // Switch to "b": counter resets, model index resets, total increments.
        state.next_channel()?;
        assert_eq!(state.current().id, "b");
        assert_eq!(state.current_model_index, 0);
        assert_eq!(state.same_channel_retries(), 0);
        assert_eq!(state.total_attempts(), 3);
        Ok(())
    }

    #[test]
    fn next_channel_returns_error_when_exhausted() -> Result<(), FailoverError> {
        let a = candidate_with_ordering("a", 0);
        let candidates: Vec<&Candidate> = vec![&a];
        let mut state = FailoverState::new(&candidates)?;

        match state.next_channel() {
            Err(FailoverError::NoMoreChannels) => {}
            Err(other) => return Err(other),
            Ok(()) => panic!("expected NoMoreChannels, got Ok"),
        }
        // current() still valid (rolled back).
        assert_eq!(state.current().id, "a");
        Ok(())
    }

    #[test]
    fn failover_walks_full_attempt_sequence() -> Result<(), FailoverError> {
        // 2 channels, max 1 same-channel retry each → a,a,b,b.
        let a = candidate_with_ordering("a", 0);
        let b = candidate_with_ordering("b", 0);
        let candidates: Vec<&Candidate> = vec![&a, &b];
        let mut state = FailoverState::new(&candidates)?;
        let policy = RetryPolicy {
            enabled: true,
            max_channel_retries: 1,
            max_single_channel_retries: 1,
            retry_delay_ms: 0,
            strategy: LoadBalancerStrategy::Adaptive,
        };

        let mut visited: Vec<&str> = vec![state.current().id.as_str()];
        loop {
            // Try same-channel retry first.
            match state.prepare_for_retry(policy, 1) {
                Ok(true) => {
                    visited.push(state.current().id.as_str());
                    continue;
                }
                Ok(false) => {}
                Err(e) => return Err(e),
            }
            // Then move to the next channel.
            match state.next_channel() {
                Ok(()) => visited.push(state.current().id.as_str()),
                Err(FailoverError::NoMoreChannels) => break,
                Err(e) => return Err(e),
            }
        }

        assert_eq!(visited, vec!["a", "a", "b", "b"]);
        assert_eq!(state.total_attempts(), 4);
        Ok(())
    }

    #[test]
    fn static_sticky_provider_returns_fixed_channel() {
        let provider = StaticStickyKeyProvider::fixed("channel-b");
        assert_eq!(
            provider.sticky_channel(Some("trace-1"), None),
            Some("channel-b".to_string())
        );
    }

    #[test]
    fn static_sticky_provider_none_returns_nothing() {
        let provider = StaticStickyKeyProvider::none();
        assert_eq!(provider.sticky_channel(Some("t"), Some("th")), None);
    }

    #[test]
    fn order_with_sticky_brings_sticky_channel_to_front() {
        let a = candidate_with_ordering("channel-a", 0);
        let b = candidate_with_ordering("channel-b", 0);
        let c = candidate_with_ordering("channel-c", 0);
        let slice: Vec<&Candidate> = vec![&a, &b, &c];
        let provider = StaticStickyKeyProvider::fixed("channel-b");

        let ordered: Vec<&str> = order_with_sticky(&slice, &provider, Some("t"), None)
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();

        // rotate_left(1) on [a,b,c] brings b to front and keeps the rest in
        // circular order: [b, c, a]. The sticky channel always lands first;
        // the remaining channels follow in their original cyclic order.
        assert_eq!(ordered, vec!["channel-b", "channel-c", "channel-a"]);
    }

    #[test]
    fn order_with_sticky_preserves_order_when_sticky_absent() {
        let a = candidate_with_ordering("channel-a", 0);
        let b = candidate_with_ordering("channel-b", 0);
        let slice: Vec<&Candidate> = vec![&a, &b];
        let provider = StaticStickyKeyProvider::fixed("channel-z");

        let ordered: Vec<&str> = order_with_sticky(&slice, &provider, Some("t"), None)
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();

        assert_eq!(ordered, vec!["channel-a", "channel-b"]);
    }

    #[test]
    fn order_with_sticky_noop_when_provider_has_no_channel() {
        let a = candidate_with_ordering("channel-a", 0);
        let b = candidate_with_ordering("channel-b", 0);
        let slice: Vec<&Candidate> = vec![&a, &b];
        let provider = StaticStickyKeyProvider::none();

        let ordered: Vec<&str> = order_with_sticky(&slice, &provider, Some("t"), None)
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();

        assert_eq!(ordered, vec!["channel-a", "channel-b"]);
    }

    #[test]
    fn select_channels_end_to_end_sort_topk_and_sticky() {
        // The sticky channel starts below the normal top-k. Trace affinity is
        // applied before truncation, matching Go's TraceAware score boost.
        let candidates = vec![
            candidate_with_ordering("low", 0),
            candidate_with_ordering("mid", 0),
            candidate_with_ordering("high", 0),
        ];
        let strategy = fixed(&[("low", 10), ("mid", 50), ("high", 90)]);
        let policy = RetryPolicy {
            enabled: true,
            max_channel_retries: 1, // top_k = 2
            max_single_channel_retries: 0,
            retry_delay_ms: 0,
            strategy: LoadBalancerStrategy::Adaptive,
        };
        let sticky = StaticStickyKeyProvider::fixed("low");

        let ordered: Vec<&str> = select_channels(
            &candidates,
            &strategy,
            policy,
            &sticky,
            Some("trace-1"),
            None,
        )
        .into_iter()
        .map(|c| c.id.as_str())
        .collect();

        assert_eq!(ordered, vec!["low", "high"]);
    }

    #[test]
    fn tie_rotation_keeps_sticky_channel_before_top_k_truncation() {
        let candidates = vec![
            candidate_with_ordering("high", 90),
            candidate_with_ordering("mid", 50),
            candidate_with_ordering("sticky", 10),
        ];
        let policy = RetryPolicy {
            enabled: false, // top_k = 1
            ..RetryPolicy::DEFAULT
        };
        let sticky = StaticStickyKeyProvider::fixed("sticky");

        let ordered: Vec<&str> = select_channels_with_tie_rotation(
            &candidates,
            &WeightScoring::new(),
            policy,
            &sticky,
            Some("trace-1"),
            None,
            0,
        )
        .into_iter()
        .map(|candidate| candidate.id.as_str())
        .collect();

        assert_eq!(ordered, vec!["sticky"]);
    }

    #[test]
    fn is_retryable_status_recognizes_default_set() {
        // 429 and all 5xx are retryable; other 4xx and 2xx are not (mirrors Go
        // httpclient.IsHTTPStatusCodeRetryable).
        for code in [429i64, 500, 501, 502, 503, 504, 599] {
            assert!(is_retryable_status(code), "{code} should be retryable");
        }
        for code in [200i64, 301, 400, 401, 403, 404, 408, 418] {
            assert!(!is_retryable_status(code), "{code} should NOT be retryable");
        }
    }

    #[test]
    fn is_retryable_status_for_channel_honors_override() {
        // 418 is not in the default set but the channel overrides it.
        assert!(is_retryable_status_for_channel(418, &[418]));
        assert!(!is_retryable_status_for_channel(418, &[]));
        // Default retryable codes still apply.
        assert!(is_retryable_status_for_channel(503, &[]));
    }

    #[test]
    fn matches_retryable_error_pattern_substring_and_regex() {
        use conduit_core::objects::channel_settings::RetryableErrorPattern;

        let patterns = vec![
            RetryableErrorPattern {
                pattern: "timeout".to_string(),
                regex: false,
            },
            RetryableErrorPattern {
                pattern: r"quota_\d+".to_string(),
                regex: true,
            },
        ];

        assert!(matches_retryable_error_pattern(
            "request timeout reached",
            &patterns
        ));
        assert!(matches_retryable_error_pattern(
            "quota_1234 exhausted",
            &patterns
        ));
        assert!(!matches_retryable_error_pattern("unauthorized", &patterns));
        // Empty message / empty patterns never match.
        assert!(!matches_retryable_error_pattern("", &patterns));
        assert!(!matches_retryable_error_pattern("timeout", &[]));
    }

    #[test]
    fn is_retryable_for_channel_combines_status_and_pattern() {
        use conduit_core::objects::channel_settings::RetryableErrorPattern;

        let status_codes = &[418i64];
        let patterns = vec![RetryableErrorPattern {
            pattern: "overloaded".to_string(),
            regex: false,
        }];

        // Default retryable status.
        assert!(is_retryable_for_channel(503, "", status_codes, &patterns));
        // Channel-overridden status.
        assert!(is_retryable_for_channel(418, "", status_codes, &patterns));
        // Matching pattern.
        assert!(is_retryable_for_channel(
            400,
            "service overloaded",
            status_codes,
            &patterns
        ));
        // Neither.
        assert!(!is_retryable_for_channel(
            400,
            "bad request",
            status_codes,
            &patterns
        ));
    }

    // -----------------------------------------------------------------------
    // New tests (RUST-P9-004): composite strategies (S05-S08) + selection
    // count (S11). Mirror Go `lb_simulation_adaptive_test.go` /
    // `lb_simulation_failover_test.go` / `lb_simuation_cb_test.go` golden
    // cases, but use fixed sub-strategy inputs so assertions are deterministic
    // (the Go `RandomStrategy` and `ModelAwareCircuitBreakerStrategy` add
    // real randomness; here we inject a fixed value).
    // -----------------------------------------------------------------------

    //--- Test helpers: closure-backed provider implementations -------------

    #[derive(Clone, Debug, Default)]
    struct MapErrorProvider(HashMap<String, ErrorAwareSnapshot>);
    impl ErrorAwareProvider for MapErrorProvider {
        fn snapshot(&self, candidate_id: &str) -> ErrorAwareSnapshot {
            self.0.get(candidate_id).copied().unwrap_or_default()
        }
    }

    #[derive(Clone, Debug, Default)]
    struct MapLatencyProvider(HashMap<String, LatencyAwareSnapshot>);
    impl LatencyAwareProvider for MapLatencyProvider {
        fn snapshot(&self, candidate_id: &str) -> LatencyAwareSnapshot {
            self.0.get(candidate_id).copied().unwrap_or_default()
        }
    }

    #[derive(Clone, Debug, Default)]
    struct MapWrrProvider(HashMap<String, WeightedRoundRobinSnapshot>);
    impl WeightedRoundRobinProvider for MapWrrProvider {
        fn snapshot(&self, candidate_id: &str) -> WeightedRoundRobinSnapshot {
            self.0
                .get(candidate_id)
                .copied()
                .unwrap_or(WeightedRoundRobinSnapshot { score: 80 })
        }
    }

    #[derive(Clone, Debug, Default)]
    struct MapCircuitBreakerProvider(HashMap<String, CircuitBreakerSnapshot>);
    impl CircuitBreakerProvider for MapCircuitBreakerProvider {
        fn snapshot(&self, candidate_id: &str, _model: &str) -> CircuitBreakerSnapshot {
            self.0.get(candidate_id).copied().unwrap_or_default()
        }
    }

    #[derive(Clone, Debug, Default)]
    struct MapRateLimitProvider(HashMap<String, RateLimitSnapshot>);
    impl RateLimitProvider for MapRateLimitProvider {
        fn snapshot(&self, candidate_id: &str) -> RateLimitSnapshot {
            self.0
                .get(candidate_id)
                .copied()
                .unwrap_or(RateLimitSnapshot { score: 100 })
        }
    }

    #[derive(Clone, Debug, Default)]
    struct MapQuotaProvider(HashMap<String, QuotaSnapshot>);
    impl QuotaProvider for MapQuotaProvider {
        fn snapshot(&self, candidate_id: &str) -> QuotaSnapshot {
            self.0
                .get(candidate_id)
                .copied()
                .unwrap_or(QuotaSnapshot { score: 0 })
        }
    }

    fn map<T: Clone>(pairs: &[(&str, T)]) -> HashMap<String, T> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    //--- ErrorAware scoring (S06 adaptive component) ----------------------

    #[test]
    fn error_aware_scoring_healthy_channel_gets_max_score() {
        // No failures → no penalty → score = maxScore (200). Mirrors Go
        // ErrorAwareStrategy with metrics.ConsecutiveFailures == 0.
        let provider = MapErrorProvider(HashMap::new());
        let strategy = ErrorAwareScoring::new(provider);
        let candidate = candidate_with_ordering("healthy", 0);

        assert_eq!(strategy.score(&candidate), 200);
    }

    #[test]
    fn error_aware_scoring_applies_consecutive_and_base_penalty() {
        // Mirror Go ErrorAware math:
        //   3 consecutive failures at full cooldown (ratio = 1.0 = 10_000 bps)
        //   penalty = 3 * 30 * 1.0 + 40 * 1.0 = 130
        //   score = 200 - 130 = 70
        let provider = MapErrorProvider(map(&[(
            "failing",
            ErrorAwareSnapshot {
                consecutive_failures: 3,
                cooldown_ratio: 10_000, // == 1.0
            },
        )]));
        let strategy = ErrorAwareScoring::new(provider);
        let candidate = candidate_with_ordering("failing", 0);

        assert_eq!(strategy.score(&candidate), 70);
    }

    #[test]
    fn error_aware_scoring_scales_by_cooldown_ratio() {
        // Half-way through cooldown (ratio = 0.5 = 5_000 bps).
        // penalty = 3 * 30 * 0.5 + 40 * 0.5 = 45 + 20 = 65
        // score = 200 - 65 = 135
        let provider = MapErrorProvider(map(&[(
            "failing",
            ErrorAwareSnapshot {
                consecutive_failures: 3,
                cooldown_ratio: 5_000,
            },
        )]));
        let strategy = ErrorAwareScoring::new(provider);
        let candidate = candidate_with_ordering("failing", 0);

        assert_eq!(strategy.score(&candidate), 135);
    }

    #[test]
    fn error_aware_scoring_clamps_to_zero_for_many_failures() {
        // 10 consecutive failures at full cooldown.
        // penalty = 10 * 30 + 40 = 340; score = 200 - 340 = -140 → 0.
        let provider = MapErrorProvider(map(&[(
            "failing",
            ErrorAwareSnapshot {
                consecutive_failures: 10,
                cooldown_ratio: 10_000,
            },
        )]));
        let strategy = ErrorAwareScoring::new(provider);
        let candidate = candidate_with_ordering("failing", 0);

        assert_eq!(strategy.score(&candidate), 0);
    }

    //--- LatencyAware scoring (S06 adaptive component) --------------------

    #[test]
    fn latency_aware_returns_neutral_when_no_signal() {
        let provider = MapLatencyProvider(HashMap::new());
        let strategy = LatencyAwareScoring::new(provider);
        let candidate = candidate_with_ordering("c", 0);

        // maxScore/2 = 80/2 = 40.
        assert_eq!(strategy.score(&candidate), 40);
    }

    #[test]
    fn latency_aware_returns_provider_score_when_signal_present() {
        let provider = MapLatencyProvider(map(&[
            (
                "fast",
                LatencyAwareSnapshot {
                    score: 75,
                    has_signal: true,
                },
            ),
            (
                "slow",
                LatencyAwareSnapshot {
                    score: 10,
                    has_signal: true,
                },
            ),
        ]));
        let strategy = LatencyAwareScoring::new(provider);

        assert_eq!(strategy.score(&candidate_with_ordering("fast", 0)), 75);
        assert_eq!(strategy.score(&candidate_with_ordering("slow", 0)), 10);
    }

    //--- WeightedRoundRobin scoring (S06 adaptive component) --------------

    #[test]
    fn weighted_round_robin_uses_provider_score() {
        let provider = MapWrrProvider(map(&[
            ("idle", WeightedRoundRobinSnapshot { score: 150 }),
            ("busy", WeightedRoundRobinSnapshot { score: 20 }),
        ]));
        let strategy = WeightedRoundRobinScoring::new(provider);

        assert_eq!(strategy.score(&candidate_with_ordering("idle", 0)), 150);
        assert_eq!(strategy.score(&candidate_with_ordering("busy", 0)), 20);
    }

    //--- Random scoring (S07 failover component) --------------------------

    #[test]
    fn random_scoring_returns_fixed_value() {
        // RandomStrategy in Go contributes 0..=0.5; tests inject a fixed bp
        // value to keep assertions deterministic.
        let strategy = RandomScoring::fixed(25); // 0.25 in bps form
        let candidate = candidate_with_ordering("c", 0);

        assert_eq!(strategy.score(&candidate), 25);
    }

    //--- ModelAwareCircuitBreaker scoring (S08 component) -----------------

    #[test]
    fn circuit_breaker_returns_neutral_score_when_no_model() {
        let provider = MapCircuitBreakerProvider(HashMap::new());
        // Empty model triggers the "no_model_specified" neutral branch.
        let strategy = ModelAwareCircuitBreakerScoring::new(provider, "");
        let candidate = candidate_with_ordering("c", 0);

        // maxScore/2 = 200/2 = 100.
        assert_eq!(strategy.score(&candidate), 100);
    }

    #[test]
    fn circuit_breaker_uses_provider_score_for_known_model() {
        // Mirror Go: a healthy channel gets effectiveWeight=1.0 → 200.
        // A half-open channel gets effectiveWeight=0.3 → 60.
        let provider = MapCircuitBreakerProvider(map(&[
            ("healthy", CircuitBreakerSnapshot { score: 200 }),
            ("half-open", CircuitBreakerSnapshot { score: 60 }),
        ]));
        let strategy = ModelAwareCircuitBreakerScoring::new(provider, "gpt-4");

        assert_eq!(strategy.score(&candidate_with_ordering("healthy", 0)), 200);
        assert_eq!(strategy.score(&candidate_with_ordering("half-open", 0)), 60);
    }

    //--- RateLimit scoring (shared by all three composites) ---------------

    #[test]
    fn rate_limit_uses_provider_score_including_exhausted_penalty() {
        let provider = MapRateLimitProvider(map(&[
            ("available", RateLimitSnapshot { score: 100 }),
            (
                "exhausted",
                RateLimitSnapshot {
                    score: -10_000, // Go rateLimitExhaustedScore
                },
            ),
        ]));
        let strategy = RateLimitScoring::new(provider);

        assert_eq!(
            strategy.score(&candidate_with_ordering("available", 0)),
            100
        );
        assert_eq!(
            strategy.score(&candidate_with_ordering("exhausted", 0)),
            -10_000
        );
    }

    //--- Quota scoring (shared by all three composites) -------------------

    #[test]
    fn quota_uses_provider_score_including_exhausted_penalty() {
        let provider = MapQuotaProvider(map(&[
            ("available", QuotaSnapshot { score: 0 }),
            (
                "exhausted",
                QuotaSnapshot {
                    score: -10_000, // Go quotaExhaustedScore
                },
            ),
        ]));
        let strategy = QuotaScoring::new(provider);

        assert_eq!(strategy.score(&candidate_with_ordering("available", 0)), 0);
        assert_eq!(
            strategy.score(&candidate_with_ordering("exhausted", 0)),
            -10_000
        );
    }

    //--- Composite: adaptive (S05/S06) ------------------------------------

    #[test]
    fn adaptive_composite_trace_sticky_overrides_weight() {
        // Mirror Go `TestAdaptiveLoadBalancer_Simulation_TraceStickyOverridesWeight`:
        // the trace's last-successful channel receives +1000 (TraceAware boost),
        // dominating every other signal.
        let candidates = vec![
            candidate_with_ordering("heavy", 80),
            candidate_with_ordering("mid", 50),
            candidate_with_ordering("sticky", 20),
            candidate_with_ordering("light", 10),
        ];

        let trace = TraceAwareScoring::for_channel("sticky");
        let error = ErrorAwareScoring::new(MapErrorProvider(HashMap::new()));
        let wrr = WeightedRoundRobinScoring::new(MapWrrProvider(map(&[
            ("heavy", WeightedRoundRobinSnapshot { score: 150 }),
            ("mid", WeightedRoundRobinSnapshot { score: 150 }),
            ("sticky", WeightedRoundRobinSnapshot { score: 150 }),
            ("light", WeightedRoundRobinSnapshot { score: 150 }),
        ])));
        let latency = LatencyAwareScoring::new(MapLatencyProvider(HashMap::new()));
        let rate_limit = RateLimitScoring::new(MapRateLimitProvider(HashMap::new()));
        let quota = QuotaScoring::new(MapQuotaProvider(HashMap::new()));

        let composite = adaptive_composite(trace, error, wrr, latency, rate_limit, quota);

        let ordered: Vec<&str> = sort_candidates_top_k(&candidates, &composite, 1)
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();

        // sticky: 1000 (trace) + 200 (error) + 150 (wrr) + 40 (latency neutral)
        //         + 100 (rate) + 0 (quota) = 1490
        // heavy:    0 (trace) + 200 + 150 + 40 + 100 + 0 = 490
        assert_eq!(ordered, vec!["sticky"]);
    }

    #[test]
    fn adaptive_composite_error_aware_migration_moves_failing_channel_last() {
        // Mirror Go `TestAdaptiveLoadBalancer_Simulation_ErrorMigrationAndRecovery`
        // (single sort step): a failing channel with 3 consecutive failures at
        // full cooldown scores ErrorAware=70 (vs 200 for healthy), pushing it
        // down even if its weight/trace score is higher.
        let candidates = vec![
            candidate_with_ordering("failing", 80),
            candidate_with_ordering("healthy", 50),
        ];

        let trace = TraceAwareScoring::for_channel("never-matches");
        let error = ErrorAwareScoring::new(MapErrorProvider(map(&[(
            "failing",
            ErrorAwareSnapshot {
                consecutive_failures: 3,
                cooldown_ratio: 10_000,
            },
        )])));
        let wrr = WeightedRoundRobinScoring::new(MapWrrProvider(map(&[
            ("failing", WeightedRoundRobinSnapshot { score: 150 }),
            ("healthy", WeightedRoundRobinSnapshot { score: 150 }),
        ])));
        let latency = LatencyAwareScoring::new(MapLatencyProvider(HashMap::new()));
        let rate_limit = RateLimitScoring::new(MapRateLimitProvider(HashMap::new()));
        let quota = QuotaScoring::new(MapQuotaProvider(HashMap::new()));

        let composite = adaptive_composite(trace, error, wrr, latency, rate_limit, quota);

        let ordered: Vec<&str> = sort_candidates_top_k(&candidates, &composite, 2)
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();

        // failing: 0 + 70 + 150 + 40 + 100 + 0 = 360
        // healthy: 0 + 200 + 150 + 40 + 100 + 0 = 490  → healthy wins
        assert_eq!(ordered, vec!["healthy", "failing"]);
    }

    #[test]
    fn adaptive_composite_rate_limit_exhausted_drops_channel_last() {
        // The -10_000 rate-limit penalty dominates every positive signal so
        // an exhausted channel still appears in top-K but ranks last (mirror
        // Go `rateLimitExhaustedScore` rationale, lb_strategy_rate_limit.go).
        let candidates = vec![
            candidate_with_ordering("exhausted", 10),
            candidate_with_ordering("healthy", 5),
        ];

        let trace = TraceAwareScoring::for_channel("never-matches");
        let error = ErrorAwareScoring::new(MapErrorProvider(HashMap::new()));
        let wrr = WeightedRoundRobinScoring::new(MapWrrProvider(HashMap::new()));
        let latency = LatencyAwareScoring::new(MapLatencyProvider(HashMap::new()));
        let rate_limit = RateLimitScoring::new(MapRateLimitProvider(map(&[(
            "exhausted",
            RateLimitSnapshot { score: -10_000 },
        )])));
        let quota = QuotaScoring::new(MapQuotaProvider(HashMap::new()));

        let composite = adaptive_composite(trace, error, wrr, latency, rate_limit, quota);

        let ordered: Vec<&str> = sort_candidates_top_k(&candidates, &composite, 2)
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();

        assert_eq!(ordered, vec!["healthy", "exhausted"]);
    }

    #[test]
    fn adaptive_composite_includes_six_sub_strategies_in_debug_breakdown() {
        // Sanity-check the composite wires exactly the Go adaptive set.
        let candidates = vec![candidate_with_ordering("c", 0)];
        let trace = TraceAwareScoring::for_channel("c");
        let error = ErrorAwareScoring::new(MapErrorProvider(HashMap::new()));
        let wrr = WeightedRoundRobinScoring::new(MapWrrProvider(HashMap::new()));
        let latency = LatencyAwareScoring::new(MapLatencyProvider(HashMap::new()));
        let rate_limit = RateLimitScoring::new(MapRateLimitProvider(HashMap::new()));
        let quota = QuotaScoring::new(MapQuotaProvider(HashMap::new()));

        let composite = adaptive_composite(trace, error, wrr, latency, rate_limit, quota);

        let (_, breakdown) = sort_candidates_with_debug(&candidates, &composite, 1);
        let row = &breakdown.scores[0];
        let names: Vec<&str> = row.components.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "TraceAware",
                "ErrorAware",
                "WeightRoundRobin",
                "LatencyAware",
                "RateLimitAware",
                "QuotaAware",
            ]
        );
    }

    //--- Composite: failover (S07) ----------------------------------------

    #[test]
    fn failover_composite_orders_by_weight_ignoring_error_state() {
        // Mirror Go `TestFailoverStrategy_Simulation`: failover is NOT
        // error-aware, so the channel with the highest ordering_weight always
        // ranks first regardless of failures. Random contributes a fixed 0
        // (deterministic) here.
        let candidates = vec![
            candidate_with_ordering("heavy", 100),
            candidate_with_ordering("mid", 50),
            candidate_with_ordering("light", 10),
        ];

        let weight = WeightScoring::new();
        let random = RandomScoring::fixed(0);
        let rate_limit = RateLimitScoring::new(MapRateLimitProvider(HashMap::new()));
        let quota = QuotaScoring::new(MapQuotaProvider(HashMap::new()));

        let composite = failover_composite(weight, random, rate_limit, quota);

        let ordered: Vec<&str> = sort_candidates_top_k(&candidates, &composite, 3)
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();

        // weight*100 = 100, 50, 10; random=0; rate_limit=100; quota=0
        // heavy=200, mid=150, light=110
        assert_eq!(ordered, vec!["heavy", "mid", "light"]);
    }

    #[test]
    fn failover_composite_random_breaks_ties_within_weight() {
        // Two channels of equal weight; Random injects a small bias toward
        // "b" (mirror Go RandomStrategy's purpose, lb_strategy_random.go).
        let candidates = vec![
            candidate_with_ordering("a", 50),
            candidate_with_ordering("b", 50),
        ];

        let weight = WeightScoring::new();
        // RandomScoring takes a fixed value; we want "b" to win, so it must
        // receive a higher random score than "a". Because RandomScoring is
        // stateless and returns one value for all candidates, we instead use
        // a tiny inline FixedScores strategy (test-only) for the random slot.
        struct RandomByCandidate(HashMap<String, i64>);
        impl ScoringStrategy for RandomByCandidate {
            fn name(&self) -> &'static str {
                "Random"
            }
            fn score(&self, c: &Candidate) -> i64 {
                self.0.get(&c.id).copied().unwrap_or(0)
            }
        }
        let random_by_candidate = RandomByCandidate(map(&[("a", 0), ("b", 25)]));

        let rate_limit = RateLimitScoring::new(MapRateLimitProvider(HashMap::new()));
        let quota = QuotaScoring::new(MapQuotaProvider(HashMap::new()));

        let composite = CompositeScoring::new()
            .with(weight)
            .with(random_by_candidate)
            .with(rate_limit)
            .with(quota);

        let ordered: Vec<&str> = sort_candidates_top_k(&candidates, &composite, 2)
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();

        // a: 50 + 0 + 100 + 0 = 150
        // b: 50 + 25 + 100 + 0 = 175  → b wins
        assert_eq!(ordered, vec!["b", "a"]);
    }

    #[test]
    fn failover_composite_rate_limit_exhausted_drops_channel_last() {
        let candidates = vec![
            candidate_with_ordering("a", 100),
            candidate_with_ordering("b", 50),
        ];

        let weight = WeightScoring::new();
        let random = RandomScoring::fixed(0);
        let rate_limit = RateLimitScoring::new(MapRateLimitProvider(map(&[(
            "a",
            RateLimitSnapshot { score: -10_000 },
        )])));
        let quota = QuotaScoring::new(MapQuotaProvider(HashMap::new()));

        let composite = failover_composite(weight, random, rate_limit, quota);

        let ordered: Vec<&str> = sort_candidates_top_k(&candidates, &composite, 2)
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();

        // a: 100 + 0 + (-10_000) + 0 = -9_900
        // b: 50 + 0 + 100 + 0 = 150  → b wins, a still present as fallback
        assert_eq!(ordered, vec!["b", "a"]);
    }

    //--- Composite: circuit-breaker (S08) ---------------------------------

    #[test]
    fn circuit_breaker_composite_orders_by_weight_when_all_healthy() {
        // Mirror Go `TestCircuitBreakerStrategy_Simulation` step 1:
        // all channels Closed (effectiveWeight=1.0 → CB score=200),
        // Weight differentiates: Ch1(100)>Ch2(50)>Ch3(10).
        let candidates = vec![
            candidate_with_ordering("ch1", 100),
            candidate_with_ordering("ch2", 50),
            candidate_with_ordering("ch3", 10),
        ];

        let weight = WeightScoring::new();
        let cb_provider = MapCircuitBreakerProvider(map(&[
            ("ch1", CircuitBreakerSnapshot { score: 200 }),
            ("ch2", CircuitBreakerSnapshot { score: 200 }),
            ("ch3", CircuitBreakerSnapshot { score: 200 }),
        ]));
        let cb = ModelAwareCircuitBreakerScoring::new(cb_provider, "gpt-4");
        let rate_limit = RateLimitScoring::new(MapRateLimitProvider(HashMap::new()));
        let quota = QuotaScoring::new(MapQuotaProvider(HashMap::new()));

        let composite = circuit_breaker_composite(weight, cb, rate_limit, quota);

        let ordered: Vec<&str> = sort_candidates_top_k(&candidates, &composite, 3)
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();

        // ch1: 100 + 200 + 100 + 0 = 400
        // ch2: 50 + 200 + 100 + 0 = 350
        // ch3: 10 + 200 + 100 + 0 = 310
        assert_eq!(ordered, vec!["ch1", "ch2", "ch3"]);
    }

    #[test]
    fn circuit_breaker_composite_half_open_channel_drops_in_ranking() {
        // Mirror Go `TestCircuitBreakerStrategy_Simulation` step 2:
        // Ch1 hits half-open (effectiveWeight=0.3 → 60), so Ch2 (higher CB
        // score) overtakes it even though Ch1 has higher weight.
        let candidates = vec![
            candidate_with_ordering("ch1", 100),
            candidate_with_ordering("ch2", 50),
        ];

        let weight = WeightScoring::new();
        let cb_provider = MapCircuitBreakerProvider(map(&[
            ("ch1", CircuitBreakerSnapshot { score: 60 }), // half-open
            ("ch2", CircuitBreakerSnapshot { score: 200 }), // closed
        ]));
        let cb = ModelAwareCircuitBreakerScoring::new(cb_provider, "gpt-4");
        let rate_limit = RateLimitScoring::new(MapRateLimitProvider(HashMap::new()));
        let quota = QuotaScoring::new(MapQuotaProvider(HashMap::new()));

        let composite = circuit_breaker_composite(weight, cb, rate_limit, quota);

        let ordered: Vec<&str> = sort_candidates_top_k(&candidates, &composite, 2)
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();

        // ch1: 100 (weight) + 60 (cb half-open) + 100 (rate) + 0 (quota) = 260
        // ch2: 50 (weight) + 200 (cb closed) + 100 (rate) + 0 (quota) = 350
        assert_eq!(ordered, vec!["ch2", "ch1"]);
    }

    #[test]
    fn circuit_breaker_composite_neutral_score_when_no_model_specified() {
        // Mirror Go `Score`: empty model returns maxScore/2 (100). All
        // channels collapse to the same CB score; weight decides.
        let candidates = vec![
            candidate_with_ordering("ch1", 100),
            candidate_with_ordering("ch2", 50),
        ];

        let weight = WeightScoring::new();
        let cb =
            ModelAwareCircuitBreakerScoring::new(MapCircuitBreakerProvider(HashMap::new()), "");
        let rate_limit = RateLimitScoring::new(MapRateLimitProvider(HashMap::new()));
        let quota = QuotaScoring::new(MapQuotaProvider(HashMap::new()));

        let composite = circuit_breaker_composite(weight, cb, rate_limit, quota);

        let ordered: Vec<&str> = sort_candidates_top_k(&candidates, &composite, 2)
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();

        // ch1: 100 + 100 + 100 + 0 = 300
        // ch2: 50 + 100 + 100 + 0 = 250
        assert_eq!(ordered, vec!["ch1", "ch2"]);
    }

    //--- Selection count (S11) --------------------------------------------

    #[test]
    fn selection_count_tracker_starts_at_zero() {
        let tracker = InMemorySelectionCounts::new();

        assert_eq!(tracker.count_for("channel-a"), 0);
        assert_eq!(tracker.count_for("channel-b"), 0);
    }

    #[test]
    fn selection_count_tracker_increment_records_per_channel() {
        let tracker = InMemorySelectionCounts::new();
        tracker.increment_selection("channel-a");
        tracker.increment_selection("channel-a");
        tracker.increment_selection("channel-b");

        assert_eq!(tracker.count_for("channel-a"), 2);
        assert_eq!(tracker.count_for("channel-b"), 1);
        assert_eq!(tracker.count_for("channel-c"), 0);
    }

    #[test]
    fn sort_top_k_with_count_increments_top_candidate_only() {
        // Mirror Go `LoadBalancer.sortProduction` lines 222-226: only
        // result[0] is incremented, regardless of how many candidates make
        // the top-K cut.
        let candidates = vec![
            candidate_with_ordering("top", 0),
            candidate_with_ordering("second", 0),
            candidate_with_ordering("third", 0),
        ];
        let strategy = fixed(&[("top", 90), ("second", 50), ("third", 10)]);
        let tracker = InMemorySelectionCounts::new();

        let ordered: Vec<&str> =
            sort_candidates_top_k_with_count(&candidates, &strategy, 3, Some(&tracker))
                .into_iter()
                .map(|c| c.id.as_str())
                .collect();

        assert_eq!(ordered, vec!["top", "second", "third"]);
        // Only "top" (the rank-0 candidate) gets the increment.
        assert_eq!(tracker.count_for("top"), 1);
        assert_eq!(tracker.count_for("second"), 0);
        assert_eq!(tracker.count_for("third"), 0);
    }

    #[test]
    fn sort_top_k_with_count_skips_increment_when_tracker_is_none() {
        // Mirror Go's nil-tracker branch (lb_simulation_adaptive_test.go
        // passes `nil` as the tracker).
        let candidates = vec![candidate_with_ordering("solo", 0)];
        let strategy = fixed(&[("solo", 10)]);

        let ordered: Vec<&str> = sort_candidates_top_k_with_count(&candidates, &strategy, 1, None)
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();

        assert_eq!(ordered, vec!["solo"]);
    }

    #[test]
    fn sort_top_k_with_count_handles_empty_candidate_list() {
        // Empty input: no top candidate, no increment, no panic.
        let candidates: Vec<Candidate> = vec![];
        let strategy = fixed(&[]);
        let tracker = InMemorySelectionCounts::new();

        let ordered = sort_candidates_top_k_with_count(&candidates, &strategy, 1, Some(&tracker));

        assert!(ordered.is_empty());
        assert_eq!(tracker.count_for("any"), 0);
    }

    #[test]
    fn sort_with_debug_and_count_also_increments_top_candidate() {
        // Mirror Go `sortWithDebug` lines 300-304: the debug path increments
        // the selection count identically to the production path.
        let candidates = vec![
            candidate_with_ordering("top", 0),
            candidate_with_ordering("second", 0),
        ];
        let strategy = fixed(&[("top", 90), ("second", 50)]);
        let tracker = InMemorySelectionCounts::new();

        let (ordered, breakdown) =
            sort_candidates_with_debug_and_count(&candidates, &strategy, 2, Some(&tracker));

        let ordered_ids: Vec<&str> = ordered.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ordered_ids, vec!["top", "second"]);
        assert_eq!(breakdown.scores.len(), 2);
        assert_eq!(tracker.count_for("top"), 1);
        assert_eq!(tracker.count_for("second"), 0);
    }

    #[test]
    fn selection_count_accumulates_across_calls_mirroring_concurrent_burst()
    -> Result<(), Box<dyn std::error::Error>> {
        // Simulate three concurrent-ish Sort calls back-to-back; the tracker
        // must reflect all three top-candidate increments. This mirrors the
        // Go rationale comment on `ChannelSelectionTracker`: "ensures
        // concurrent/burst requests don't all select the same channel".
        let candidates = vec![
            candidate_with_ordering("preferred", 0),
            candidate_with_ordering("other", 0),
        ];
        let strategy = fixed(&[("preferred", 100), ("other", 10)]);
        let tracker = InMemorySelectionCounts::new();

        for _ in 0..3 {
            sort_candidates_top_k_with_count(&candidates, &strategy, 1, Some(&tracker));
        }

        let preferred = tracker.count_for("preferred");
        let other = tracker.count_for("other");
        assert_eq!(preferred, 3);
        assert_eq!(other, 0);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Pure-logic scoring formulas (TODO RUST-P9-004 refinement):
    // weighted-round-robin + latency math. These mirror the Go
    // `*_test.go` golden numbers (`TestRoundRobinStrategy_Score_*`,
    // `TestWeightRoundRobinStrategy_Score_*`,
    // `TestLatencyAwareStrategy_Score_*`) so a future Rust
    // `ChannelMetricsProvider` can reuse the helpers and stay Go-parity.
    // -----------------------------------------------------------------------

    #[test]
    fn clamp_normalized_maps_linearly_into_unit_range() {
        // Mirrors Go `clampNormalized` over [min=5, max=100].
        assert!((clamp_normalized(5.0, 5.0, 100.0) - 0.0).abs() < 1e-9);
        assert!((clamp_normalized(100.0, 5.0, 100.0) - 1.0).abs() < 1e-9);
        assert!((clamp_normalized(52.5, 5.0, 100.0) - 0.5).abs() < 1e-9);
        // Out-of-range values clamp to [0,1].
        assert!((clamp_normalized(-10.0, 5.0, 100.0) - 0.0).abs() < 1e-9);
        assert!((clamp_normalized(200.0, 5.0, 100.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn clamp_normalized_returns_zero_when_max_le_min() {
        // Go early-returns 0 when `max <= min`.
        assert_eq!(clamp_normalized(50.0, 100.0, 100.0), 0.0);
        assert_eq!(clamp_normalized(50.0, 100.0, 50.0), 0.0);
    }

    #[test]
    fn clamp_normalized_inverse_inverts_and_clamps() {
        // Mirrors Go `clampNormalizedInverse` with max=3000.
        assert!((clamp_normalized_inverse(0.0, 3000.0) - 1.0).abs() < 1e-9);
        assert!((clamp_normalized_inverse(3000.0, 3000.0) - 0.0).abs() < 1e-9);
        assert!((clamp_normalized_inverse(1500.0, 3000.0) - 0.5).abs() < 1e-9);
        // Above max → 0; below 0 → 1.
        assert_eq!(clamp_normalized_inverse(5000.0, 3000.0), 0.0);
        assert_eq!(clamp_normalized_inverse(-10.0, 3000.0), 1.0);
    }

    #[test]
    fn clamp_normalized_inverse_returns_zero_when_max_le_zero() {
        assert_eq!(clamp_normalized_inverse(100.0, 0.0), 0.0);
        assert_eq!(clamp_normalized_inverse(100.0, -5.0), 0.0);
    }

    //--- Round-robin score (mirrors TestRoundRobinStrategy_Score_*) --------

    #[test]
    fn round_robin_score_zero_requests_is_max() {
        // Go `TestRoundRobinStrategy_Score_ZeroRequests`: RequestCount=0 → 150.
        let score = round_robin_score(RoundRobinFormulaInput {
            request_count: 0,
            inactivity_secs: 0.0,
        });
        assert!((score - 150.0).abs() < 1e-9);
    }

    #[test]
    fn round_robin_score_low_requests_stays_between_100_and_150() {
        // Go `TestRoundRobinStrategy_Score_LowRequests`: 10 requests →
        // 150 * exp(-10/150) ≈ 140.48, in (100, 150).
        let score = round_robin_score(RoundRobinFormulaInput {
            request_count: 10,
            inactivity_secs: 0.0,
        });
        assert!(score > 100.0, "low requests should score >100, got {score}");
        assert!(score < 150.0, "low requests should score <150, got {score}");
    }

    #[test]
    fn round_robin_score_moderate_requests_in_70_to_80() {
        // Go `TestRoundRobinStrategy_Score_ModerateRequests`: 100 requests →
        // 150 * exp(-100/150) ≈ 77.0, in (70, 80).
        let score = round_robin_score(RoundRobinFormulaInput {
            request_count: 100,
            inactivity_secs: 0.0,
        });
        assert!(score > 70.0, "moderate should score >70, got {score}");
        assert!(score < 80.0, "moderate should score <80, got {score}");
        // Spot-check the exact Go formula: 150 * exp(-100/150) ≈ 77.0486.
        let expected = 150.0 * std::f64::consts::E.powf(-100.0 / 150.0);
        assert!(
            (score - expected).abs() < 1e-9,
            "exact Go formula, got {score}"
        );
    }

    #[test]
    fn round_robin_score_high_requests_clamps_to_min() {
        // Go `TestRoundRobinStrategy_Score_HighRequests`: 500 requests →
        // raw ≈ 5.35, clamped to minScore = 10.
        let score = round_robin_score(RoundRobinFormulaInput {
            request_count: 500,
            inactivity_secs: 0.0,
        });
        assert!(
            (score - 10.0).abs() < 1e-9,
            "clamped to minScore, got {score}"
        );
    }

    #[test]
    fn round_robin_score_caps_request_count_at_1000() {
        // Go `TestRoundRobinStrategy_Score_CappedRequests`: 2000 requests
        // behave like 1000 (the cap) → score in [10, 20].
        let score = round_robin_score(RoundRobinFormulaInput {
            request_count: 2000,
            inactivity_secs: 0.0,
        });
        assert!(score >= 10.0, "never below minScore, got {score}");
        assert!(score <= 20.0, "very high usage stays low, got {score}");
    }

    #[test]
    fn round_robin_score_inactivity_decay_recovers_score_for_idle_channel() {
        // Go `TestRoundRobinStrategy_Score_InactivityDecay`: 500 requests,
        // 10 minutes idle (decay factor exp(-600/300) ≈ 0.135) → effective
        // ≈ 67.5 → score ≈ 96, well above the active channel's clamped 10.
        let active = round_robin_score(RoundRobinFormulaInput {
            request_count: 500,
            inactivity_secs: 0.0,
        });
        let idle = round_robin_score(RoundRobinFormulaInput {
            request_count: 500,
            inactivity_secs: 600.0,
        });
        assert!(
            active < 20.0,
            "active channel stays near floor, got {active}"
        );
        assert!(idle > 80.0, "idle channel recovers, got {idle}");
        assert!(idle > active, "idle outranks active");
    }

    //--- Weighted round-robin score (mirrors TestWeightRoundRobinStrategy_*) --

    #[test]
    fn weighted_round_robin_score_zero_requests_is_max_regardless_of_weight() {
        // Go `TestWeightRoundRobinStrategy_Score_ZeroRequests`: 0 requests
        // → maxScore for every weight.
        for weight in [0, 25, 50, 100] {
            let score = weighted_round_robin_score(
                RoundRobinFormulaInput {
                    request_count: 0,
                    inactivity_secs: 0.0,
                },
                weight,
            );
            assert!(
                (score - 150.0).abs() < 1e-9,
                "weight={weight} zero requests should give max, got {score}"
            );
        }
    }

    #[test]
    fn weighted_round_robin_score_normalizes_by_weight() {
        // Go `TestWeightRoundRobinStrategy_Score_ModerateRequests` golden
        // ranges (100 requests):
        //   weight=0  → normalized=100 → ~77.0  (70..=85)
        //   weight=25 → normalized=400 → ~10.04 (10..=11)
        //   weight=50 → normalized=200 → ~39.6  (35..=45)
        //   weight=100→ normalized=100 → ~77.0  (70..=85)
        let cases = [
            (0_i64, 70.0_f64, 85.0_f64),
            (25, 10.0, 11.0),
            (50, 35.0, 45.0),
            (100, 70.0, 85.0),
        ];
        for (weight, lo, hi) in cases {
            let score = weighted_round_robin_score(
                RoundRobinFormulaInput {
                    request_count: 100,
                    inactivity_secs: 0.0,
                },
                weight,
            );
            assert!(
                score >= lo && score <= hi,
                "weight={weight} expected in [{lo},{hi}], got {score}"
            );
        }
    }

    #[test]
    fn weighted_round_robin_score_high_requests_hits_soft_clamp() {
        // Go `TestWeightRoundRobinStrategy_Score_HighRequests`: 500 requests,
        // weight=50 → normalized=1000 → raw ≈ 0.18 → soft clamp ≈ 10.001,
        // asserted within 0.1 of 10.0.
        let score = weighted_round_robin_score(
            RoundRobinFormulaInput {
                request_count: 500,
                inactivity_secs: 0.0,
            },
            50,
        );
        assert!(
            (score - 10.0).abs() < 0.1,
            "soft-clamped near minScore, got {score}"
        );
        // Soft clamp is strictly above minScore (Go: minScore + raw/maxScore).
        assert!(score > 10.0, "soft clamp > minScore, got {score}");
    }

    #[test]
    fn weighted_round_robin_score_proportional_requests_collapse_onto_same_score() {
        // Go `TestWeightRoundRobinStrategy_MultipleChannels`: when each
        // channel's `request_count == weight` (so normalized=100 for all),
        // the scores collapse onto the same value (~77).
        let inputs = [(80_i64, 80_i64), (50, 50), (10, 10)];
        let mut scores = Vec::new();
        for (count, weight) in inputs {
            scores.push(weighted_round_robin_score(
                RoundRobinFormulaInput {
                    request_count: count,
                    inactivity_secs: 0.0,
                },
                weight,
            ));
        }
        assert!(
            (scores[0] - scores[1]).abs() < 1.0,
            "proportional collapse 0-1"
        );
        assert!(
            (scores[1] - scores[2]).abs() < 1.0,
            "proportional collapse 1-2"
        );
    }

    #[test]
    fn weighted_round_robin_score_inactivity_decay_recovers_idle_channel() {
        // Go `TestWeightRoundRobinStrategy_Score_InactivityDecay`: weight=100,
        // 400 requests. Active → normalized=400 → ~10.2. Idle (10min) →
        // effective ≈ 54 → score ≈ 105.
        let active = weighted_round_robin_score(
            RoundRobinFormulaInput {
                request_count: 400,
                inactivity_secs: 0.0,
            },
            100,
        );
        let idle = weighted_round_robin_score(
            RoundRobinFormulaInput {
                request_count: 400,
                inactivity_secs: 600.0,
            },
            100,
        );
        assert!(active < 20.0, "active stays near floor, got {active}");
        assert!(idle > 80.0, "idle recovers, got {idle}");
        assert!(idle > active);
    }

    //--- Latency-aware score (mirrors TestLatencyAwareStrategy_Score_*) ----

    #[test]
    fn latency_streaming_score_matches_go_golden_value() {
        // Go `TestLatencyAwareStrategy_Score_StreamingUsesFirstTokenAndTPS`:
        // first_token=300, tps=60, samples=10 →
        // 0.7*(1-300/3000) + 0.3*((60-5)/(100-5)) = 0.803684...
        // score = 80 * 0.803684 ≈ 64.2947.
        let score = streaming_latency_score(LatencyFormulaInput {
            streaming_first_token_latency_ewma_ms: 300.0,
            streaming_tokens_per_second_ewma: 60.0,
            streaming_sample_count: 10,
            non_streaming_latency_ewma_ms: 0.0,
            non_streaming_sample_count: 0,
        });
        assert!(score.has_signal, "samples>0 → signal present");
        assert!(
            (score.score - 64.29).abs() < 0.01,
            "streaming golden ≈64.29, got {}",
            score.score
        );
        assert!(
            (resolve_latency_score(score) - 64.29).abs() < 0.01,
            "resolve applies signal score"
        );
    }

    #[test]
    fn latency_streaming_score_prefers_better_first_token_latency() {
        // Go `TestLatencyAwareStrategy_Score_StreamingPrefersBetterFTTL`:
        // a 200ms first-token channel outranks a 1200ms one even though the
        // slower channel has higher throughput.
        let fast = streaming_latency_score(LatencyFormulaInput {
            streaming_first_token_latency_ewma_ms: 200.0,
            streaming_tokens_per_second_ewma: 40.0,
            streaming_sample_count: 10,
            non_streaming_latency_ewma_ms: 0.0,
            non_streaming_sample_count: 0,
        });
        let slow = streaming_latency_score(LatencyFormulaInput {
            streaming_first_token_latency_ewma_ms: 1200.0,
            streaming_tokens_per_second_ewma: 70.0,
            streaming_sample_count: 10,
            non_streaming_latency_ewma_ms: 0.0,
            non_streaming_sample_count: 0,
        });
        assert!(fast.score > slow.score, "first-token latency dominates");
    }

    #[test]
    fn latency_streaming_score_no_samples_returns_no_signal() {
        let score = streaming_latency_score(LatencyFormulaInput {
            streaming_sample_count: 0,
            ..Default::default()
        });
        assert!(!score.has_signal);
        // Resolve falls back to neutral maxScore/2 = 40.
        assert!((resolve_latency_score(score) - 40.0).abs() < 1e-9);
    }

    #[test]
    fn latency_streaming_score_zero_tps_uses_neutral_throughput_component() {
        // Go: `tps == 0` → throughput_score = 0.5. With first_token=0
        // (perfect) the formula yields 80*(0.7*1 + 0.3*0.5) = 68.0.
        let score = streaming_latency_score(LatencyFormulaInput {
            streaming_first_token_latency_ewma_ms: 0.0,
            streaming_tokens_per_second_ewma: 0.0,
            streaming_sample_count: 5,
            non_streaming_latency_ewma_ms: 0.0,
            non_streaming_sample_count: 0,
        });
        assert!(score.has_signal);
        assert!(
            (score.score - 68.0).abs() < 1e-9,
            "zero-tps neutral throughput, got {}",
            score.score
        );
    }

    #[test]
    fn latency_non_streaming_score_matches_go_golden_value() {
        // Go `TestLatencyAwareStrategy_Score_NonStreamingUsesTotalLatency`:
        // latency=1200, samples=8 → 80 * (1 - 1200/3000) = 80 * 0.6 = 48.0.
        let score = non_streaming_latency_score(LatencyFormulaInput {
            non_streaming_latency_ewma_ms: 1200.0,
            non_streaming_sample_count: 8,
            streaming_first_token_latency_ewma_ms: 0.0,
            streaming_tokens_per_second_ewma: 0.0,
            streaming_sample_count: 0,
        });
        assert!(score.has_signal);
        assert!(
            (score.score - 48.0).abs() < 0.01,
            "non-streaming golden ≈48.0, got {}",
            score.score
        );
    }

    #[test]
    fn latency_non_streaming_score_no_samples_returns_no_signal() {
        let score = non_streaming_latency_score(LatencyFormulaInput {
            non_streaming_sample_count: 0,
            ..Default::default()
        });
        assert!(!score.has_signal);
        assert!((resolve_latency_score(score) - 40.0).abs() < 1e-9);
    }

    // -----------------------------------------------------------------------
    // RUST-P15-001: lb_strategy golden cases (random / weight / composite /
    // latency). Mirror the Go `*_test.go` golden assertions not already covered
    // by the tests above. Each test cites the Go test it mirrors so a future
    // contract audit can trace the parity.
    // -----------------------------------------------------------------------

    //--- lb_strategy_random_test.go --------------------------------------

    #[test]
    fn random_strategy_name_is_random() {
        // Mirrors Go TestRandomStrategy_Name (lb_strategy_random_test.go:47).
        let strategy = RandomScoring::fixed(0);
        assert_eq!(strategy.name(), "Random");
    }

    #[test]
    fn random_strategy_score_stays_in_go_default_range() {
        // Mirrors Go TestRandomStrategy_Score (lb_strategy_random_test.go:12):
        // Go draws 100 random values and asserts each is in [0, 0.5]. The Rust
        // port takes the value as a parameter (deterministic), so we assert the
        // range invariant over every value in the Go default range expressed in
        // basis points (0..=50 == [0.0, 0.5]).
        let candidate = candidate_with_ordering("c", 0);
        for value_bps in 0..=50_i64 {
            let strategy = RandomScoring::fixed(value_bps);
            let score = strategy.score(&candidate);
            assert!(
                (0..=50).contains(&score),
                "Random score {score} outside Go default [0, 0.5] range"
            );
        }
    }

    #[test]
    fn random_strategy_score_with_debug_name_and_value() {
        // Mirrors Go TestRandomStrategy_ScoreWithDebug (lb_strategy_random_test.go:32):
        // asserts StrategyName=="Random", Score==score, and details map. Rust
        // uses ScoreComponent (not a details map), so we assert name + value.
        let strategy = RandomScoring::fixed(25);
        let candidate = candidate_with_ordering("c", 0);
        let (score, debug) = strategy.score_with_debug(&candidate);
        assert_eq!(score, 25);
        assert_eq!(debug.len(), 1);
        assert_eq!(debug[0].name, "Random");
        assert_eq!(debug[0].value, 25);
    }

    //--- lb_strategy_weight_test.go --------------------------------------

    #[test]
    fn weight_strategy_name_is_weight() {
        // Mirrors Go TestWeightStrategy_Name (lb_strategy_weight_test.go:66).
        let strategy = WeightScoring::new();
        assert_eq!(strategy.name(), "Weight");
    }

    #[test]
    fn weight_strategy_score_low_weight_in_expected_range() {
        // Mirrors Go TestWeightStrategy_Score "low weight" sub-case
        // (lb_strategy_weight_test.go:30-34): weight=25 → score in [24, 26].
        // (weight_scoring_normalizes_ordering_weight above already covers the
        // zero / medium / high sub-cases.)
        let strategy = WeightScoring::new();
        let candidate = candidate_with_ordering("c", 25);
        let score = strategy.score(&candidate);
        assert!(
            (24..=26).contains(&score),
            "weight=25 expected score in [24,26], got {score}"
        );
    }

    #[test]
    fn weight_strategy_score_equals_score_with_debug_for_all_weights() {
        // Mirrors Go TestWeightStrategy_ScoreConsistency (lb_strategy_weight_test.go:71):
        // Score and ScoreWithDebug must return identical scores across the full
        // weight range including negative.
        let strategy = WeightScoring::new();
        for weight in [-10_i64, 0, 25, 50, 100] {
            let candidate = candidate_with_ordering("c", weight);
            let score = strategy.score(&candidate);
            let (debug_score, _) = strategy.score_with_debug(&candidate);
            assert_eq!(
                score, debug_score,
                "Score != ScoreWithDebug for weight={weight}"
            );
        }
    }

    #[test]
    fn cost_aware_scoring_applies_configured_weight_and_reports_component() {
        let candidate = Candidate::new("cheap", "provider", "model", CandidateStatus::Ready)
            .with_routing_cost(Some("0.001250".to_string()), 800);
        let strategy = CostAwareScoring::new(Arc::new(WeightScoring::new()), 25);

        let (score, components) = strategy.score_with_debug(&candidate);

        assert_eq!(score, 200);
        assert_eq!(strategy.score(&candidate), score);
        assert!(components.iter().any(|component| {
            component.name == "theoretical_cost_efficiency" && component.value == 200
        }));
    }

    //--- lb_strategy_composite_test.go -----------------------------------

    /// A strategy that returns a fixed score for any candidate, mirroring Go's
    /// `mockStrategy{name, score}` (lb_strategies_test.go:12).
    struct MockStrategy {
        name: &'static str,
        score: i64,
    }
    impl ScoringStrategy for MockStrategy {
        fn name(&self) -> &'static str {
            self.name
        }
        fn score(&self, _candidate: &Candidate) -> i64 {
            self.score
        }
    }

    #[test]
    fn composite_strategy_name_is_composite() {
        // Mirrors Go TestCompositeStrategy_Name (lb_strategy_composite_test.go:46).
        let composite = CompositeScoring::new();
        assert_eq!(composite.name(), "Composite");
    }

    #[test]
    fn composite_strategy_sums_mock_strategy_scores() {
        // Mirrors Go TestCompositeStrategy_Score (lb_strategy_composite_test.go:13):
        // s1=100 + s2=50 with default weights (1.0 each) → 150.
        let composite = CompositeScoring::new()
            .with(MockStrategy {
                name: "s1",
                score: 100,
            })
            .with(MockStrategy {
                name: "s2",
                score: 50,
            });
        let candidate = candidate_with_ordering("c", 0);
        assert_eq!(composite.score(&candidate), 150);
    }

    #[test]
    fn composite_strategy_with_weights_applies_weighted_sum() {
        // Mirrors Go TestCompositeStrategy_WithWeights (lb_strategy_composite_test.go:29):
        // (100 * 2.0) + (50 * 0.5) = 200 + 25 = 225.
        let composite = CompositeScoring::new()
            .with(MockStrategy {
                name: "s1",
                score: 100,
            })
            .with(MockStrategy {
                name: "s2",
                score: 50,
            })
            .with_weights(&[2.0, 0.5]);
        let candidate = candidate_with_ordering("c", 0);
        assert_eq!(composite.score(&candidate), 225);
    }

    #[test]
    fn composite_strategy_score_equals_score_with_debug_for_all_weight_configs() {
        // Mirrors Go TestCompositeStrategy_ScoreConsistency (lb_strategy_composite_test.go:51):
        // Score == ScoreWithDebug for default / custom / zero weights.
        let candidate = candidate_with_ordering("c", 0);
        let cases: &[(&str, &[f64])] = &[
            ("default", &[]),
            ("custom", &[2.0, 0.5]),
            ("zero", &[0.0, 0.0]),
        ];
        for (label, weights) in cases {
            let mut composite = CompositeScoring::new()
                .with(MockStrategy {
                    name: "s1",
                    score: 100,
                })
                .with(MockStrategy {
                    name: "s2",
                    score: 50,
                });
            if !weights.is_empty() {
                composite = composite.with_weights(weights);
            }
            let score = composite.score(&candidate);
            let (debug_score, _) = composite.score_with_debug(&candidate);
            assert_eq!(score, debug_score, "{label}: Score != ScoreWithDebug");
        }
    }

    //--- lb_strategy_latency_test.go -------------------------------------

    #[test]
    fn latency_aware_strategy_name_is_latency_aware() {
        // Mirrors Go TestLatencyAwareStrategy_Name (lb_strategy_latency_test.go:13).
        let strategy = LatencyAwareScoring::new(MapLatencyProvider(HashMap::new()));
        assert_eq!(strategy.name(), "LatencyAware");
    }

    #[test]
    fn latency_aware_score_with_debug_returns_name_and_signal_score() {
        // Mirrors Go TestLatencyAwareStrategy_ScoreWithDebug_Streaming
        // (lb_strategy_latency_test.go:100): asserts StrategyName=="LatencyAware"
        // and Score in the debug path. Rust uses ScoreComponent (not a Go-style
        // details map), so we assert name + value. Score 58 ≈ Go's 58.04 golden
        // (first_token=500, tps=50, samples=20); the EWMA math itself is
        // exercised by latency_streaming_score_matches_go_golden_value above.
        let provider = MapLatencyProvider(map(&[(
            "c",
            LatencyAwareSnapshot {
                score: 58,
                has_signal: true,
            },
        )]));
        let strategy = LatencyAwareScoring::new(provider);
        let candidate = candidate_with_ordering("c", 0);
        let (score, debug) = strategy.score_with_debug(&candidate);
        assert_eq!(score, 58);
        assert_eq!(debug.len(), 1);
        assert_eq!(debug[0].name, "LatencyAware");
        assert_eq!(debug[0].value, 58);
    }

    #[test]
    fn latency_aware_score_with_debug_no_signal_returns_neutral() {
        // Mirrors Go TestLatencyAwareStrategy_ScoreWithDebug_MetricsError
        // (lb_strategy_latency_test.go:125): when the provider has no data,
        // the debug path returns maxScore/2 = 40. Go maps a provider error to
        // the same neutral fallback; the Rust provider trait returns a default
        // (no-signal) snapshot instead of an error, so the neutral result is
        // identical.
        let strategy = LatencyAwareScoring::new(MapLatencyProvider(HashMap::new()));
        let candidate = candidate_with_ordering("c", 0);
        let (score, debug) = strategy.score_with_debug(&candidate);
        assert_eq!(score, 40); // LATENCY_MAX_SCORE / 2
        assert_eq!(debug.len(), 1);
        assert_eq!(debug[0].name, "LatencyAware");
        assert_eq!(debug[0].value, 40);
    }
}
