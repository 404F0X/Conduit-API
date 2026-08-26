//! Project-scoped commercial policy.
//!
//! This module is the compatibility boundary between the current runtime
//! user-group rules and the target account model. It deliberately contains no
//! database access: callers can dual-evaluate legacy and project policies
//! before switching the request path.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use thiserror::Error;

/// One neutral multiplier in parts per million.
pub const MULTIPLIER_ONE_PPM: i64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectEntitlementEffect {
    Grant,
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectEntitlementOverride {
    pub id: i64,
    pub public_model_id: i64,
    pub effect: ProjectEntitlementEffect,
    pub source_type: String,
    pub source_id: Option<String>,
    pub status: String,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_until: Option<DateTime<Utc>>,
}

impl ProjectEntitlementOverride {
    pub fn is_active_at(&self, at: DateTime<Utc>) -> bool {
        self.status == "active"
            && self.valid_from.is_none_or(|start| start <= at)
            && self.valid_until.is_none_or(|end| at < end)
    }
}

/// Explainable result for one public model. Blocks always win over grants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectEntitlementDecision {
    pub public_model_id: i64,
    pub allowed: bool,
    pub allowed_by_base_plan: bool,
    pub active_grant_ids: Vec<i64>,
    pub active_block_ids: Vec<i64>,
}

/// One immutable Project-entitlement evaluation snapshot.
///
/// Catalog rendering, API-key admission, and request execution must evaluate
/// the same base plan and override set at the same instant. Keeping that state
/// in one resolver prevents each caller from subtly reimplementing
/// `(Base Access Plan ∪ Active Grants) - Explicit Blocks`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveProjectEntitlementResolver {
    base_plan_model_ids: BTreeSet<i64>,
    overrides: Vec<ProjectEntitlementOverride>,
    evaluated_at: DateTime<Utc>,
}

/// Explainable batch result. Decisions include denied models so callers can
/// surface or audit the same reason that request admission used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveProjectEntitlements {
    pub evaluated_at: DateTime<Utc>,
    pub allowed_model_ids: BTreeSet<i64>,
    pub decisions: BTreeMap<i64, ProjectEntitlementDecision>,
}

impl EffectiveProjectEntitlementResolver {
    pub fn new(
        base_plan_model_ids: BTreeSet<i64>,
        overrides: Vec<ProjectEntitlementOverride>,
        evaluated_at: DateTime<Utc>,
    ) -> Self {
        Self {
            base_plan_model_ids,
            overrides,
            evaluated_at,
        }
    }

    pub fn evaluated_at(&self) -> DateTime<Utc> {
        self.evaluated_at
    }

    pub fn base_plan_model_ids(&self) -> &BTreeSet<i64> {
        &self.base_plan_model_ids
    }

    pub fn overrides(&self) -> &[ProjectEntitlementOverride] {
        &self.overrides
    }

    pub fn decision(&self, public_model_id: i64) -> ProjectEntitlementDecision {
        resolve_project_entitlement_at(
            public_model_id,
            &self.base_plan_model_ids,
            &self.overrides,
            self.evaluated_at,
        )
    }

    pub fn resolve(
        &self,
        public_model_ids: impl IntoIterator<Item = i64>,
    ) -> EffectiveProjectEntitlements {
        let decisions = public_model_ids
            .into_iter()
            .map(|public_model_id| (public_model_id, self.decision(public_model_id)))
            .collect::<BTreeMap<_, _>>();
        let allowed_model_ids = decisions
            .values()
            .filter(|decision| decision.allowed)
            .map(|decision| decision.public_model_id)
            .collect();
        EffectiveProjectEntitlements {
            evaluated_at: self.evaluated_at,
            allowed_model_ids,
            decisions,
        }
    }
}

pub fn resolve_project_entitlement(
    public_model_id: i64,
    base_plan_model_ids: &BTreeSet<i64>,
    overrides: &[ProjectEntitlementOverride],
    at: DateTime<Utc>,
) -> ProjectEntitlementDecision {
    resolve_project_entitlement_at(public_model_id, base_plan_model_ids, overrides, at)
}

fn resolve_project_entitlement_at(
    public_model_id: i64,
    base_plan_model_ids: &BTreeSet<i64>,
    overrides: &[ProjectEntitlementOverride],
    at: DateTime<Utc>,
) -> ProjectEntitlementDecision {
    let mut grants = Vec::new();
    let mut blocks = Vec::new();

    for rule in overrides
        .iter()
        .filter(|rule| rule.public_model_id == public_model_id && rule.is_active_at(at))
    {
        match rule.effect {
            ProjectEntitlementEffect::Grant => grants.push(rule.id),
            ProjectEntitlementEffect::Block => blocks.push(rule.id),
        }
    }
    grants.sort_unstable();
    blocks.sort_unstable();

    let allowed_by_base_plan = base_plan_model_ids.contains(&public_model_id);
    ProjectEntitlementDecision {
        public_model_id,
        allowed: blocks.is_empty() && (allowed_by_base_plan || !grants.is_empty()),
        allowed_by_base_plan,
        active_grant_ids: grants,
        active_block_ids: blocks,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPriceAdjustment {
    pub id: i64,
    pub multiplier_ppm: i64,
    pub stacking_key: String,
    pub priority: i32,
    pub source_type: String,
    pub source_id: Option<String>,
    pub status: String,
    pub valid_from: Option<DateTime<Utc>>,
    pub valid_until: Option<DateTime<Utc>>,
}

impl ProjectPriceAdjustment {
    pub fn is_active_at(&self, at: DateTime<Utc>) -> bool {
        self.status == "active"
            && self.valid_from.is_none_or(|start| start <= at)
            && self.valid_until.is_none_or(|end| at < end)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPriceDecision {
    pub effective_multiplier_ppm: i64,
    pub applied_adjustment_ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProjectPriceError {
    #[error("multiplier must be non-negative")]
    NegativeMultiplier,
    #[error("multiplier calculation overflowed")]
    Overflow,
}

/// Resolve one base price tier plus at most one winner per stacking key.
///
/// Winners are ordered by `(priority, id)`, so the result is stable even when
/// the database returns rows in a different order.
pub fn resolve_project_price_multiplier(
    base_multiplier_ppm: i64,
    adjustments: &[ProjectPriceAdjustment],
    at: DateTime<Utc>,
) -> Result<ProjectPriceDecision, ProjectPriceError> {
    if base_multiplier_ppm < 0 {
        return Err(ProjectPriceError::NegativeMultiplier);
    }

    let mut winners: BTreeMap<&str, &ProjectPriceAdjustment> = BTreeMap::new();
    for adjustment in adjustments.iter().filter(|item| item.is_active_at(at)) {
        if adjustment.multiplier_ppm < 0 {
            return Err(ProjectPriceError::NegativeMultiplier);
        }
        winners
            .entry(adjustment.stacking_key.as_str())
            .and_modify(|current| {
                if (adjustment.priority, adjustment.id) > (current.priority, current.id) {
                    *current = adjustment;
                }
            })
            .or_insert(adjustment);
    }

    let mut multiplier = i128::from(base_multiplier_ppm);
    let mut applied_ids = Vec::with_capacity(winners.len());
    for winner in winners.values() {
        multiplier = multiplier
            .checked_mul(i128::from(winner.multiplier_ppm))
            .ok_or(ProjectPriceError::Overflow)?;
        // Round half up at every explicitly ordered stacking boundary.
        multiplier = multiplier
            .checked_add(i128::from(MULTIPLIER_ONE_PPM / 2))
            .ok_or(ProjectPriceError::Overflow)?
            / i128::from(MULTIPLIER_ONE_PPM);
        applied_ids.push(winner.id);
    }

    let effective_multiplier_ppm =
        i64::try_from(multiplier).map_err(|_| ProjectPriceError::Overflow)?;
    applied_ids.sort_unstable();
    Ok(ProjectPriceDecision {
        effective_multiplier_ppm,
        applied_adjustment_ids: applied_ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::<Utc>::UNIX_EPOCH + chrono::TimeDelta::seconds(1_785_830_400)
    }

    fn entitlement(
        id: i64,
        model: i64,
        effect: ProjectEntitlementEffect,
    ) -> ProjectEntitlementOverride {
        ProjectEntitlementOverride {
            id,
            public_model_id: model,
            effect,
            source_type: "admin".to_owned(),
            source_id: None,
            status: "active".to_owned(),
            valid_from: None,
            valid_until: None,
        }
    }

    #[test]
    fn base_plan_plus_grant_minus_block() {
        let base = BTreeSet::from([1, 2]);
        let rules = vec![
            entitlement(10, 3, ProjectEntitlementEffect::Grant),
            entitlement(11, 2, ProjectEntitlementEffect::Block),
            entitlement(12, 3, ProjectEntitlementEffect::Block),
        ];

        assert!(resolve_project_entitlement(1, &base, &rules, now()).allowed);
        assert!(!resolve_project_entitlement(2, &base, &rules, now()).allowed);
        let model_three = resolve_project_entitlement(3, &base, &rules, now());
        assert!(!model_three.allowed, "an explicit block must beat a grant");
        assert_eq!(model_three.active_grant_ids, vec![10]);
        assert_eq!(model_three.active_block_ids, vec![12]);
    }

    #[test]
    fn inactive_and_expired_overrides_do_not_apply() {
        let mut disabled = entitlement(1, 7, ProjectEntitlementEffect::Grant);
        disabled.status = "disabled".to_owned();
        let mut expired = entitlement(2, 7, ProjectEntitlementEffect::Grant);
        expired.valid_until = Some(now());

        let decision =
            resolve_project_entitlement(7, &BTreeSet::new(), &[disabled, expired], now());
        assert!(!decision.allowed);
        assert!(decision.active_grant_ids.is_empty());
    }

    #[test]
    fn effective_resolver_batches_one_explainable_snapshot() {
        let evaluated_at = now();
        let resolver = EffectiveProjectEntitlementResolver::new(
            BTreeSet::from([1, 2]),
            vec![
                entitlement(10, 2, ProjectEntitlementEffect::Block),
                entitlement(11, 3, ProjectEntitlementEffect::Grant),
                entitlement(12, 3, ProjectEntitlementEffect::Block),
                entitlement(13, 4, ProjectEntitlementEffect::Grant),
            ],
            evaluated_at,
        );

        let effective = resolver.resolve([4, 3, 2, 1, 4]);

        assert_eq!(effective.evaluated_at, evaluated_at);
        assert_eq!(effective.allowed_model_ids, BTreeSet::from([1, 4]));
        assert_eq!(
            effective.decisions.len(),
            4,
            "duplicate ids are evaluated once"
        );
        assert!(effective.decisions[&1].allowed_by_base_plan);
        assert_eq!(effective.decisions[&2].active_block_ids, vec![10]);
        assert_eq!(effective.decisions[&3].active_grant_ids, vec![11]);
        assert_eq!(effective.decisions[&3].active_block_ids, vec![12]);
        assert_eq!(effective.decisions[&4].active_grant_ids, vec![13]);
    }

    #[test]
    fn resolver_uses_construction_time_for_every_decision() {
        let mut future_grant = entitlement(20, 5, ProjectEntitlementEffect::Grant);
        future_grant.valid_from = Some(now() + chrono::Duration::seconds(1));
        let resolver =
            EffectiveProjectEntitlementResolver::new(BTreeSet::new(), vec![future_grant], now());

        assert!(!resolver.decision(5).allowed);
        assert_eq!(resolver.resolve([5]).evaluated_at, now());
    }

    fn adjustment(
        id: i64,
        key: &str,
        priority: i32,
        multiplier_ppm: i64,
    ) -> ProjectPriceAdjustment {
        ProjectPriceAdjustment {
            id,
            multiplier_ppm,
            stacking_key: key.to_owned(),
            priority,
            source_type: "promotion".to_owned(),
            source_id: None,
            status: "active".to_owned(),
            valid_from: None,
            valid_until: None,
        }
    }

    #[test]
    fn price_uses_one_deterministic_winner_per_stacking_key() -> Result<(), ProjectPriceError> {
        let adjustments = vec![
            adjustment(1, "partner", 10, 900_000),
            adjustment(2, "partner", 20, 800_000),
            adjustment(3, "promotion", 5, 500_000),
        ];

        let decision = resolve_project_price_multiplier(MULTIPLIER_ONE_PPM, &adjustments, now())?;
        assert_eq!(decision.effective_multiplier_ppm, 400_000);
        assert_eq!(decision.applied_adjustment_ids, vec![2, 3]);
        Ok(())
    }

    #[test]
    fn later_id_breaks_equal_priority_ties() -> Result<(), ProjectPriceError> {
        let adjustments = vec![
            adjustment(4, "tier", 10, 700_000),
            adjustment(5, "tier", 10, 600_000),
        ];
        let decision = resolve_project_price_multiplier(MULTIPLIER_ONE_PPM, &adjustments, now())?;
        assert_eq!(decision.effective_multiplier_ppm, 600_000);
        assert_eq!(decision.applied_adjustment_ids, vec![5]);
        Ok(())
    }
}
