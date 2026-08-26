use std::collections::BTreeMap;

use conduit_services::{ProviderQuotaEnforcementMode, ProviderQuotaStatus};

use crate::candidate::Candidate;

pub const DE_PRIORITIZED_PROVIDER_WEIGHT: u32 = 1;

pub fn apply_provider_quota(
    candidates: &[Candidate],
    statuses: &[ProviderQuotaStatus],
    mode: ProviderQuotaEnforcementMode,
) -> Vec<Candidate> {
    let statuses_by_channel: BTreeMap<&str, &ProviderQuotaStatus> = statuses
        .iter()
        .map(|status| (status.channel_id.as_str(), status))
        .collect();

    match mode {
        ProviderQuotaEnforcementMode::ExhaustedOnly => candidates
            .iter()
            .filter(|candidate| !is_exhausted(candidate, &statuses_by_channel))
            .cloned()
            .collect(),
        ProviderQuotaEnforcementMode::DePrioritize => candidates
            .iter()
            .cloned()
            .map(|mut candidate| {
                if is_exhausted(&candidate, &statuses_by_channel) {
                    candidate.weight = de_prioritized_weight(candidate.weight);
                }
                candidate
            })
            .collect(),
    }
}

fn is_exhausted(
    candidate: &Candidate,
    statuses_by_channel: &BTreeMap<&str, &ProviderQuotaStatus>,
) -> bool {
    statuses_by_channel
        .get(candidate.id.as_str())
        .is_some_and(|status| status.is_exhausted())
}

const fn de_prioritized_weight(weight: u32) -> u32 {
    if weight == 0 {
        0
    } else if weight > DE_PRIORITIZED_PROVIDER_WEIGHT {
        DE_PRIORITIZED_PROVIDER_WEIGHT
    } else {
        weight
    }
}

#[cfg(test)]
mod tests {
    use conduit_services::{PROVIDER_QUOTA_STATUS_EXHAUSTED, PROVIDER_QUOTA_STATUS_READY};

    use super::*;
    use crate::candidate::{Candidate, CandidateStatus};

    fn candidate(id: &str, weight: u32) -> Candidate {
        Candidate::new(id, "synthetic", "gpt-test", CandidateStatus::Ready).with_weight(weight)
    }

    #[test]
    fn exhausted_only_filters_exhausted_candidates() {
        let candidates = vec![candidate("channel-a", 10), candidate("channel-b", 10)];
        let statuses = vec![
            ProviderQuotaStatus::exhausted("channel-a", "synthetic"),
            ProviderQuotaStatus::new("channel-b", "synthetic"),
        ];

        let selected = apply_provider_quota(
            &candidates,
            &statuses,
            ProviderQuotaEnforcementMode::ExhaustedOnly,
        );

        assert_eq!(selected, vec![candidate("channel-b", 10)]);
    }

    #[test]
    fn de_prioritize_lowers_exhausted_candidate_weight() {
        let candidates = vec![candidate("channel-a", 10), candidate("channel-b", 7)];
        let statuses = vec![ProviderQuotaStatus::exhausted("channel-a", "synthetic")];

        let selected = apply_provider_quota(
            &candidates,
            &statuses,
            ProviderQuotaEnforcementMode::DePrioritize,
        );

        assert_eq!(selected[0].id, "channel-a");
        assert_eq!(selected[0].weight, DE_PRIORITIZED_PROVIDER_WEIGHT);
        assert_eq!(selected[1], candidate("channel-b", 7));
    }

    #[test]
    fn non_exhausted_ready_and_status_fields_do_not_filter() {
        let candidates = vec![
            Candidate::new(
                "channel-a",
                "synthetic",
                "gpt-test",
                CandidateStatus::Degraded,
            )
            .with_weight(5),
            candidate("channel-b", 5),
        ];
        let statuses = vec![ProviderQuotaStatus {
            status: PROVIDER_QUOTA_STATUS_READY.to_string(),
            ready: false,
            ..ProviderQuotaStatus::new("channel-a", "synthetic")
        }];

        let selected = apply_provider_quota(
            &candidates,
            &statuses,
            ProviderQuotaEnforcementMode::ExhaustedOnly,
        );

        assert_eq!(selected, candidates);
        assert_eq!(selected[0].status, CandidateStatus::Degraded);
        assert_eq!(statuses[0].status, PROVIDER_QUOTA_STATUS_READY);
        assert_ne!(statuses[0].status, PROVIDER_QUOTA_STATUS_EXHAUSTED);
    }
}
