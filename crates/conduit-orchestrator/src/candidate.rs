use std::collections::{BTreeMap, BTreeSet};

use conduit_llm::RequestType;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub provider: String,
    pub model: String,
    pub tags: Vec<String>,
    pub enabled: bool,
    pub status: CandidateStatus,
    pub weight: u32,
    /// Priority tie-breaker mirroring Go `Channel.OrderingWeight`. Higher value
    /// wins when strategy scores are equal (Go `partial.SortFunc` tie-break).
    /// Defaults to 0. **Not** the load-balancing `weight` — that is [`Self::weight`].
    pub ordering_weight: i64,
    pub theoretical_cost_accounting: Option<String>,
    pub cost_efficiency_score: i64,
}

impl Candidate {
    pub fn new(
        id: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
        status: CandidateStatus,
    ) -> Self {
        Self {
            id: id.into(),
            provider: provider.into(),
            model: model.into(),
            tags: Vec::new(),
            enabled: true,
            status,
            weight: 1,
            ordering_weight: 0,
            theoretical_cost_accounting: None,
            cost_efficiency_score: 0,
        }
    }

    /// Set the priority tie-breaker (Go `Channel.OrderingWeight`).
    pub const fn with_ordering_weight(mut self, ordering_weight: i64) -> Self {
        self.ordering_weight = ordering_weight;
        self
    }

    pub fn with_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }

    pub const fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    pub const fn archived(mut self) -> Self {
        self.status = CandidateStatus::Archived;
        self
    }

    pub const fn with_weight(mut self, weight: u32) -> Self {
        self.weight = weight;
        self
    }

    pub fn with_routing_cost(mut self, theoretical_cost: Option<String>, score: i64) -> Self {
        self.theoretical_cost_accounting = theoretical_cost;
        self.cost_efficiency_score = score.clamp(0, 1000);
        self
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CandidateCacheSignature {
    pub tags: Vec<String>,
    pub profile_signature: Option<String>,
}

impl CandidateCacheSignature {
    pub fn new(
        tags: impl IntoIterator<Item = impl Into<String>>,
        profile_signature: Option<impl Into<String>>,
    ) -> Self {
        let mut tags: Vec<String> = tags.into_iter().map(Into::into).collect();
        tags.sort();
        tags.dedup();

        Self {
            tags,
            profile_signature: profile_signature.map(Into::into),
        }
    }

    pub fn tags_only(tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::new(tags, Option::<String>::None)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CandidateCacheKey {
    pub project_id: String,
    pub api_key_id: String,
    pub model: String,
    pub request_type: RequestType,
    pub stream: bool,
    pub signature: CandidateCacheSignature,
    pub channel_update_version: u64,
    pub model_update_version: u64,
}

impl CandidateCacheKey {
    pub fn new(
        project_id: impl Into<String>,
        api_key_id: impl Into<String>,
        model: impl Into<String>,
        request_type: RequestType,
        stream: bool,
        signature: CandidateCacheSignature,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            api_key_id: api_key_id.into(),
            model: model.into(),
            request_type,
            stream,
            signature,
            channel_update_version: 0,
            model_update_version: 0,
        }
    }

    pub const fn with_channel_update_version(mut self, version: u64) -> Self {
        self.channel_update_version = version;
        self
    }

    pub const fn with_model_update_version(mut self, version: u64) -> Self {
        self.model_update_version = version;
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidateStatus {
    Ready,
    Degraded,
    Unavailable,
    Archived,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CandidateFilterReasonCode {
    Disabled,
    Archived,
    EnabledMismatch,
    StatusMismatch,
    ModelMismatch,
    MissingRequiredTag,
    Quota,
    RateLimit,
    CircuitBreaker,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateFilterReason {
    pub candidate_id: String,
    pub provider: String,
    pub model: String,
    pub code: CandidateFilterReasonCode,
    pub detail: CandidateFilterReasonDetail,
}

impl CandidateFilterReason {
    fn for_candidate(
        candidate: &Candidate,
        code: CandidateFilterReasonCode,
        detail: CandidateFilterReasonDetail,
    ) -> Self {
        Self {
            candidate_id: candidate.id.clone(),
            provider: candidate.provider.clone(),
            model: candidate.model.clone(),
            code,
            detail,
        }
    }

    fn disabled(candidate: &Candidate) -> Self {
        Self::for_candidate(
            candidate,
            CandidateFilterReasonCode::Disabled,
            CandidateFilterReasonDetail::Disabled,
        )
    }

    fn archived(candidate: &Candidate) -> Self {
        Self::for_candidate(
            candidate,
            CandidateFilterReasonCode::Archived,
            CandidateFilterReasonDetail::Archived,
        )
    }

    fn enabled_mismatch(candidate: &Candidate, expected: bool) -> Self {
        Self::for_candidate(
            candidate,
            CandidateFilterReasonCode::EnabledMismatch,
            CandidateFilterReasonDetail::EnabledMismatch {
                expected,
                actual: candidate.enabled,
            },
        )
    }

    fn status_mismatch(candidate: &Candidate, expected: &[CandidateStatus]) -> Self {
        Self::for_candidate(
            candidate,
            CandidateFilterReasonCode::StatusMismatch,
            CandidateFilterReasonDetail::StatusMismatch {
                expected: expected.to_vec(),
                actual: candidate.status,
            },
        )
    }

    fn model_mismatch(candidate: &Candidate, expected: &str) -> Self {
        Self::for_candidate(
            candidate,
            CandidateFilterReasonCode::ModelMismatch,
            CandidateFilterReasonDetail::ModelMismatch {
                expected: expected.to_string(),
                actual: candidate.model.clone(),
            },
        )
    }

    fn missing_required_tag(candidate: &Candidate, tag: &str) -> Self {
        Self::for_candidate(
            candidate,
            CandidateFilterReasonCode::MissingRequiredTag,
            CandidateFilterReasonDetail::MissingRequiredTag {
                tag: tag.to_string(),
            },
        )
    }

    pub fn quota(candidate: &Candidate, scope: impl Into<String>) -> Self {
        Self::for_candidate(
            candidate,
            CandidateFilterReasonCode::Quota,
            CandidateFilterReasonDetail::Quota {
                scope: scope.into(),
            },
        )
    }

    pub fn rate_limit(
        candidate: &Candidate,
        scope: impl Into<String>,
        retry_after_ticks: Option<u64>,
    ) -> Self {
        Self::for_candidate(
            candidate,
            CandidateFilterReasonCode::RateLimit,
            CandidateFilterReasonDetail::RateLimit {
                scope: scope.into(),
                retry_after_ticks,
            },
        )
    }

    pub fn circuit_breaker(candidate: &Candidate, state: impl Into<String>) -> Self {
        Self::for_candidate(
            candidate,
            CandidateFilterReasonCode::CircuitBreaker,
            CandidateFilterReasonDetail::CircuitBreaker {
                state: state.into(),
            },
        )
    }

    pub fn other(candidate: &Candidate, message: impl Into<String>) -> Self {
        Self::for_candidate(
            candidate,
            CandidateFilterReasonCode::Other,
            CandidateFilterReasonDetail::Other {
                message: message.into(),
            },
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CandidateFilterReasonDetail {
    Disabled,
    Archived,
    EnabledMismatch {
        expected: bool,
        actual: bool,
    },
    StatusMismatch {
        expected: Vec<CandidateStatus>,
        actual: CandidateStatus,
    },
    ModelMismatch {
        expected: String,
        actual: String,
    },
    MissingRequiredTag {
        tag: String,
    },
    Quota {
        scope: String,
    },
    RateLimit {
        scope: String,
        retry_after_ticks: Option<u64>,
    },
    CircuitBreaker {
        state: String,
    },
    Other {
        message: String,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CandidateFilterDiagnostics {
    pub selected: Vec<CandidateFilterDiagnosticCandidate>,
    pub rejected: Vec<CandidateFilterReason>,
}

impl CandidateFilterDiagnostics {
    pub fn summary(&self) -> CandidateFilterSummary {
        CandidateFilterSummary::from_diagnostics(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateFilterDiagnosticCandidate {
    pub candidate_id: String,
    pub provider: String,
    pub model: String,
}

impl CandidateFilterDiagnosticCandidate {
    fn from_candidate(candidate: &Candidate) -> Self {
        Self {
            candidate_id: candidate.id.clone(),
            provider: candidate.provider.clone(),
            model: candidate.model.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CandidateFilterSummary {
    pub total_candidates: usize,
    pub selected_count: usize,
    pub rejected_candidate_count: usize,
    pub rejection_count: usize,
    pub reason_counts: Vec<CandidateFilterReasonCount>,
}

impl CandidateFilterSummary {
    pub fn from_diagnostics(diagnostics: &CandidateFilterDiagnostics) -> Self {
        let mut rejected_candidate_ids = BTreeSet::new();
        let mut reason_counts = BTreeMap::new();

        for reason in &diagnostics.rejected {
            rejected_candidate_ids.insert(reason.candidate_id.as_str());
            *reason_counts.entry(reason.code).or_insert(0) += 1;
        }

        Self {
            total_candidates: diagnostics.selected.len() + rejected_candidate_ids.len(),
            selected_count: diagnostics.selected.len(),
            rejected_candidate_count: rejected_candidate_ids.len(),
            rejection_count: diagnostics.rejected.len(),
            reason_counts: reason_counts
                .into_iter()
                .map(|(code, count)| CandidateFilterReasonCount { code, count })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateFilterReasonCount {
    pub code: CandidateFilterReasonCode,
    pub count: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CandidateFilter {
    pub enabled: Option<bool>,
    pub statuses: Vec<CandidateStatus>,
    pub model: Option<String>,
    pub required_tags: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CandidateSelectionContext {
    pub sticky_channel_hint: Option<StickyChannelHint>,
}

impl CandidateSelectionContext {
    pub fn with_sticky_channel_hint(mut self, hint: StickyChannelHint) -> Self {
        self.sticky_channel_hint = Some(hint);
        self
    }

    pub fn sticky_match(&self, candidate: &Candidate) -> Option<CandidateStickyMatch> {
        self.sticky_channel_hint
            .as_ref()
            .filter(|hint| hint.channel_id == candidate.id)
            .map(|hint| CandidateStickyMatch {
                channel_id: hint.channel_id.clone(),
                trace_id: hint.trace_id.clone(),
                thread_id: hint.thread_id.clone(),
            })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StickyChannelHint {
    pub channel_id: String,
    pub trace_id: Option<String>,
    pub thread_id: Option<String>,
}

impl StickyChannelHint {
    pub fn new(channel_id: impl Into<String>) -> Self {
        Self {
            channel_id: channel_id.into(),
            trace_id: None,
            thread_id: None,
        }
    }

    pub fn for_trace(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }

    pub fn for_thread(mut self, thread_id: impl Into<String>) -> Self {
        self.thread_id = Some(thread_id.into());
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateStickyMatch {
    pub channel_id: String,
    pub trace_id: Option<String>,
    pub thread_id: Option<String>,
}

impl CandidateFilter {
    pub fn ready_for(model: impl Into<String>) -> Self {
        Self {
            enabled: Some(true),
            statuses: vec![CandidateStatus::Ready],
            model: Some(model.into()),
            required_tags: Vec::new(),
        }
    }

    pub fn with_required_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.required_tags = tags.into_iter().map(Into::into).collect();
        self
    }

    fn matches(&self, candidate: &Candidate) -> bool {
        self.rejection_reasons(candidate).is_empty()
    }

    fn rejection_reasons(&self, candidate: &Candidate) -> Vec<CandidateFilterReason> {
        let mut reasons = Vec::new();

        if let Some(enabled) = self.enabled
            && candidate.enabled != enabled
        {
            if !candidate.enabled {
                reasons.push(CandidateFilterReason::disabled(candidate));
            } else {
                reasons.push(CandidateFilterReason::enabled_mismatch(candidate, enabled));
            }
        }

        if !self.statuses.is_empty() && !self.statuses.contains(&candidate.status) {
            if candidate.status == CandidateStatus::Archived {
                reasons.push(CandidateFilterReason::archived(candidate));
            } else {
                reasons.push(CandidateFilterReason::status_mismatch(
                    candidate,
                    &self.statuses,
                ));
            }
        }

        if let Some(model) = &self.model
            && candidate.model != *model
        {
            reasons.push(CandidateFilterReason::model_mismatch(candidate, model));
        }

        // Tags are treated as required capabilities: every requested tag must exist.
        reasons.extend(
            self.required_tags
                .iter()
                .filter(|tag| !candidate.tags.contains(tag))
                .map(|tag| CandidateFilterReason::missing_required_tag(candidate, tag)),
        );

        reasons
    }
}

pub trait CandidateSelector {
    fn select<'a>(
        &self,
        candidates: &'a [Candidate],
        filter: &CandidateFilter,
    ) -> Vec<&'a Candidate>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FilteringCandidateSelector;

impl CandidateSelector for FilteringCandidateSelector {
    fn select<'a>(
        &self,
        candidates: &'a [Candidate],
        filter: &CandidateFilter,
    ) -> Vec<&'a Candidate> {
        candidates
            .iter()
            .filter(|candidate| filter.matches(candidate))
            .collect()
    }
}

impl FilteringCandidateSelector {
    pub fn diagnose(
        &self,
        candidates: &[Candidate],
        filter: &CandidateFilter,
    ) -> CandidateFilterDiagnostics {
        let mut diagnostics = CandidateFilterDiagnostics::default();

        for candidate in candidates {
            let reasons = filter.rejection_reasons(candidate);

            if reasons.is_empty() {
                diagnostics
                    .selected
                    .push(CandidateFilterDiagnosticCandidate::from_candidate(
                        candidate,
                    ));
            } else {
                diagnostics.rejected.extend(reasons);
            }
        }

        diagnostics
    }

    pub fn select_with_context<'a>(
        &self,
        candidates: &'a [Candidate],
        filter: &CandidateFilter,
        context: &CandidateSelectionContext,
    ) -> Vec<&'a Candidate> {
        let mut selected = self.select(candidates, filter);

        if let Some(sticky_channel_id) = context
            .sticky_channel_hint
            .as_ref()
            .map(|hint| hint.channel_id.as_str())
            && let Some(index) = selected
                .iter()
                .position(|candidate| candidate.id == sticky_channel_id)
        {
            selected.rotate_left(index);
        }

        selected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, model: &str) -> Candidate {
        Candidate::new(id, "provider-a", model, CandidateStatus::Ready)
    }

    fn cache_key() -> CandidateCacheKey {
        CandidateCacheKey::new(
            "project-a",
            "api-key-a",
            "gpt-4o",
            RequestType::Chat,
            false,
            CandidateCacheSignature::new(["fast", "vision"], Some("profile-a")),
        )
    }

    #[test]
    fn candidate_cache_key_separates_request_dimensions() {
        let base = cache_key();

        assert_ne!(
            base,
            CandidateCacheKey::new(
                "project-b",
                "api-key-a",
                "gpt-4o",
                RequestType::Chat,
                false,
                CandidateCacheSignature::new(["fast", "vision"], Some("profile-a")),
            )
        );
        assert_ne!(
            base,
            CandidateCacheKey::new(
                "project-a",
                "api-key-b",
                "gpt-4o",
                RequestType::Chat,
                false,
                CandidateCacheSignature::new(["fast", "vision"], Some("profile-a")),
            )
        );
        assert_ne!(
            base,
            CandidateCacheKey::new(
                "project-a",
                "api-key-a",
                "gpt-4o-mini",
                RequestType::Chat,
                false,
                CandidateCacheSignature::new(["fast", "vision"], Some("profile-a")),
            )
        );
        assert_ne!(
            base,
            CandidateCacheKey::new(
                "project-a",
                "api-key-a",
                "gpt-4o",
                RequestType::Chat,
                true,
                CandidateCacheSignature::new(["fast", "vision"], Some("profile-a")),
            )
        );
        assert_ne!(
            base,
            CandidateCacheKey::new(
                "project-a",
                "api-key-a",
                "gpt-4o",
                RequestType::Chat,
                false,
                CandidateCacheSignature::new(["fast"], Some("profile-a")),
            )
        );
        assert_ne!(
            base,
            CandidateCacheKey::new(
                "project-a",
                "api-key-a",
                "gpt-4o",
                RequestType::Chat,
                false,
                CandidateCacheSignature::new(["fast", "vision"], Some("profile-b")),
            )
        );
    }

    #[test]
    fn candidate_cache_signature_sorts_tags_stably() {
        assert_eq!(
            CandidateCacheSignature::new(["vision", "fast", "fast"], Some("profile-a")),
            CandidateCacheSignature {
                tags: vec!["fast".to_string(), "vision".to_string()],
                profile_signature: Some("profile-a".to_string()),
            }
        );

        assert_eq!(
            CandidateCacheSignature::tags_only(["vision", "fast"]),
            CandidateCacheSignature::tags_only(["fast", "vision"]),
        );
    }

    #[test]
    fn candidate_cache_key_versions_are_part_of_identity() {
        let base = cache_key();

        assert_ne!(base, cache_key().with_channel_update_version(1));
        assert_ne!(base, cache_key().with_model_update_version(1));
    }

    #[test]
    fn disabled_candidates_are_filtered_out() {
        let candidates = vec![
            candidate("enabled", "gpt-4o"),
            candidate("disabled", "gpt-4o").disabled(),
        ];
        let filter = CandidateFilter::ready_for("gpt-4o");

        let selected = FilteringCandidateSelector.select(&candidates, &filter);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, "enabled");
    }

    #[test]
    fn model_and_tag_filters_must_match() {
        let candidates = vec![
            candidate("match", "gpt-4o").with_tags(["vision", "fast"]),
            candidate("wrong-model", "gpt-4o-mini").with_tags(["vision", "fast"]),
            candidate("missing-tag", "gpt-4o").with_tags(["vision"]),
        ];
        let filter = CandidateFilter::ready_for("gpt-4o").with_required_tags(["vision", "fast"]);

        let selected = FilteringCandidateSelector.select(&candidates, &filter);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, "match");
    }

    #[test]
    fn status_filter_allows_only_requested_statuses() {
        let candidates = vec![
            candidate("ready", "gpt-4o"),
            Candidate::new(
                "degraded",
                "provider-a",
                "gpt-4o",
                CandidateStatus::Degraded,
            ),
            Candidate::new(
                "unavailable",
                "provider-a",
                "gpt-4o",
                CandidateStatus::Unavailable,
            ),
        ];
        let filter = CandidateFilter {
            enabled: Some(true),
            statuses: vec![CandidateStatus::Ready, CandidateStatus::Degraded],
            model: Some("gpt-4o".to_string()),
            required_tags: Vec::new(),
        };

        let selected = FilteringCandidateSelector.select(&candidates, &filter);
        let ids: Vec<&str> = selected
            .iter()
            .map(|candidate| candidate.id.as_str())
            .collect();

        assert_eq!(ids, vec!["ready", "degraded"]);
    }

    #[test]
    fn filter_diagnostics_reports_structured_rejection_reasons() {
        let candidates = vec![
            candidate("match", "gpt-4o").with_tags(["vision", "fast"]),
            candidate("disabled", "gpt-4o")
                .with_tags(["vision", "fast"])
                .disabled(),
            candidate("archived", "gpt-4o")
                .with_tags(["vision", "fast"])
                .archived(),
            candidate("wrong-model", "gpt-4o-mini").with_tags(["vision", "fast"]),
            candidate("missing-tag", "gpt-4o").with_tags(["vision"]),
        ];
        let filter = CandidateFilter::ready_for("gpt-4o").with_required_tags(["vision", "fast"]);

        let diagnostics = FilteringCandidateSelector.diagnose(&candidates, &filter);
        let summary = diagnostics.summary();

        assert_eq!(
            diagnostics.selected,
            vec![CandidateFilterDiagnosticCandidate {
                candidate_id: "match".to_string(),
                provider: "provider-a".to_string(),
                model: "gpt-4o".to_string(),
            }]
        );
        assert_eq!(
            diagnostics
                .rejected
                .iter()
                .map(|reason| reason.code)
                .collect::<Vec<_>>(),
            vec![
                CandidateFilterReasonCode::Disabled,
                CandidateFilterReasonCode::Archived,
                CandidateFilterReasonCode::ModelMismatch,
                CandidateFilterReasonCode::MissingRequiredTag,
            ]
        );
        assert_eq!(
            diagnostics.rejected[2].detail,
            CandidateFilterReasonDetail::ModelMismatch {
                expected: "gpt-4o".to_string(),
                actual: "gpt-4o-mini".to_string(),
            }
        );
        assert_eq!(
            summary,
            CandidateFilterSummary {
                total_candidates: 5,
                selected_count: 1,
                rejected_candidate_count: 4,
                rejection_count: 4,
                reason_counts: vec![
                    CandidateFilterReasonCount {
                        code: CandidateFilterReasonCode::Disabled,
                        count: 1,
                    },
                    CandidateFilterReasonCount {
                        code: CandidateFilterReasonCode::Archived,
                        count: 1,
                    },
                    CandidateFilterReasonCount {
                        code: CandidateFilterReasonCode::ModelMismatch,
                        count: 1,
                    },
                    CandidateFilterReasonCount {
                        code: CandidateFilterReasonCode::MissingRequiredTag,
                        count: 1,
                    },
                ],
            }
        );
    }

    #[test]
    fn filter_diagnostics_can_report_multiple_reasons_for_one_candidate() {
        let candidates = vec![candidate("blocked", "gpt-4o-mini")];
        let filter = CandidateFilter::ready_for("gpt-4o").with_required_tags(["vision", "fast"]);

        let diagnostics = FilteringCandidateSelector.diagnose(&candidates, &filter);

        assert_eq!(diagnostics.summary().total_candidates, 1);
        assert_eq!(diagnostics.summary().rejected_candidate_count, 1);
        assert_eq!(diagnostics.summary().rejection_count, 3);
        assert_eq!(
            diagnostics
                .rejected
                .iter()
                .map(|reason| reason.code)
                .collect::<Vec<_>>(),
            vec![
                CandidateFilterReasonCode::ModelMismatch,
                CandidateFilterReasonCode::MissingRequiredTag,
                CandidateFilterReasonCode::MissingRequiredTag,
            ]
        );
    }

    #[test]
    fn runtime_filter_reasons_are_summarized_without_db_state() {
        let candidate = candidate("channel-a", "gpt-4o");
        let diagnostics = CandidateFilterDiagnostics {
            selected: Vec::new(),
            rejected: vec![
                CandidateFilterReason::quota(&candidate, "provider-daily"),
                CandidateFilterReason::rate_limit(&candidate, "channel-rpm", Some(30)),
                CandidateFilterReason::circuit_breaker(&candidate, "open"),
            ],
        };

        assert_eq!(
            diagnostics.rejected[1].detail,
            CandidateFilterReasonDetail::RateLimit {
                scope: "channel-rpm".to_string(),
                retry_after_ticks: Some(30),
            }
        );
        assert_eq!(
            diagnostics.summary().reason_counts,
            vec![
                CandidateFilterReasonCount {
                    code: CandidateFilterReasonCode::Quota,
                    count: 1,
                },
                CandidateFilterReasonCount {
                    code: CandidateFilterReasonCode::RateLimit,
                    count: 1,
                },
                CandidateFilterReasonCount {
                    code: CandidateFilterReasonCode::CircuitBreaker,
                    count: 1,
                },
            ]
        );
    }

    #[test]
    fn sticky_channel_hint_marks_matching_candidate_context() {
        let context = CandidateSelectionContext::default().with_sticky_channel_hint(
            StickyChannelHint::new("channel-b")
                .for_trace("trace-1")
                .for_thread("thread-1"),
        );
        let candidate = candidate("channel-b", "gpt-4o");

        let sticky_match = context.sticky_match(&candidate);

        assert_eq!(
            sticky_match,
            Some(CandidateStickyMatch {
                channel_id: "channel-b".to_string(),
                trace_id: Some("trace-1".to_string()),
                thread_id: Some("thread-1".to_string()),
            })
        );
    }

    #[test]
    fn sticky_channel_hint_prioritizes_available_candidate() {
        let candidates = vec![
            candidate("channel-a", "gpt-4o"),
            candidate("channel-b", "gpt-4o"),
        ];
        let filter = CandidateFilter::ready_for("gpt-4o");
        let context = CandidateSelectionContext::default()
            .with_sticky_channel_hint(StickyChannelHint::new("channel-b").for_trace("trace-1"));

        let selected =
            FilteringCandidateSelector.select_with_context(&candidates, &filter, &context);
        let ids: Vec<&str> = selected
            .iter()
            .map(|candidate| candidate.id.as_str())
            .collect();

        assert_eq!(ids, vec!["channel-b", "channel-a"]);
    }

    #[test]
    fn sticky_channel_hint_falls_back_when_candidate_is_unavailable() {
        let candidates = vec![
            Candidate::new(
                "channel-a",
                "provider-a",
                "gpt-4o",
                CandidateStatus::Unavailable,
            ),
            candidate("channel-b", "gpt-4o"),
        ];
        let filter = CandidateFilter::ready_for("gpt-4o");
        let context = CandidateSelectionContext::default()
            .with_sticky_channel_hint(StickyChannelHint::new("channel-a").for_thread("thread-1"));

        let selected =
            FilteringCandidateSelector.select_with_context(&candidates, &filter, &context);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, "channel-b");
    }
}
