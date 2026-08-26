//! ChannelModelPrice repository — trait + input types for the
//! `channel_model_prices` head table and its `channel_model_price_versions`
//! history.
//!
//! ## Go contract (source of truth)
//! `conduit/internal/ent/schema/channel_model_price.go` +
//! `channel_model_price_versions.go`, driven by
//! `conduit/internal/server/biz/channel_price.go`
//! (`SaveChannelModelPrices` / `calculatePriceChanges`) and
//! `channel_duplicate.go` (`DuplicateChannel`).
//!
//! - `ChannelModelPrice`: TimeMixin + SoftDeleteMixin. Fields `channel_id`
//!   (int, immutable), `model_id` (string, immutable), `price`
//!   (JSON `objects.ModelPrice`, NOT NULL), `reference_id` (unique string —
//!   regenerated whenever the price changes). Unique index
//!   `(channel_id, model_id, deleted_at)`.
//! - `ChannelModelPriceVersion`: TimeMixin only (immutable price history, so
//!   NO soft delete). Fields add `channel_model_price_id` (int edge),
//!   `status` (`active`|`archived`), `effective_start_at`,
//!   `effective_end_at` (nullable), and its own unique `reference_id`.
//!
//! ## Version bookkeeping (Go `SaveChannelModelPrices`)
//! On an update or delete, the active version(s) for the head row are archived
//! (`status active→archived`, `effective_end_at = now`); on a create or update
//! a fresh `active` version is written with a new `reference_id`. This trait
//! exposes the primitives — `archive_active_versions` + `create_version` — so
//! the wiring layer can replay the Go action sequence.
//!
//! ## Policy guard
//! Go schema `Policy()`: OwnerRule + read/write **channels**-scope rules — a
//! global (not project-scoped) surface. The Rust analog is the crate-standard
//! `guard_repo_principal(ctx)` on every checked wrapper (anonymous callers are
//! rejected before any SQL runs).

use async_trait::async_trait;
use serde_json::Value;

use crate::repo::{RepoResult, RequestContext, guard_repo_principal};
use crate::row::{ChannelModelPriceRow, ChannelModelPriceVersionRow};

/// Version status values (Go `channelmodelpriceversion.Status{Active,Archived}`).
pub const VERSION_STATUS_ACTIVE: &str = "active";
pub const VERSION_STATUS_ARCHIVED: &str = "archived";

/// Repository surface for the channel-model-price head table + version history.
/// Checked methods apply the crate-standard principal guard (mirrors the Go
/// schema `Policy()`); `*_unchecked` methods are the backend implementations.
#[async_trait]
pub trait ChannelModelPriceRepo: Send + Sync {
    async fn list_prices_by_channel_unchecked(
        &self,
        ctx: &RequestContext,
        channel_id: i64,
    ) -> RepoResult<Vec<ChannelModelPriceRow>>;

    /// All live (non-deleted) prices for a channel, id ASC (mirrors ent's
    /// default order used by Go's `ChannelModelPrice.Query().Where(ChannelID)`).
    async fn list_prices_by_channel(
        &self,
        ctx: &RequestContext,
        channel_id: i64,
    ) -> RepoResult<Vec<ChannelModelPriceRow>> {
        guard_repo_principal(ctx)?;
        self.list_prices_by_channel_unchecked(ctx, channel_id).await
    }

    async fn create_price_unchecked(
        &self,
        ctx: &RequestContext,
        channel_id: i64,
        model_id: &str,
        currency_code: &str,
        price: &Value,
        reference_id: &str,
        now: &str,
    ) -> RepoResult<ChannelModelPriceRow>;

    /// Insert a new head row (Go `ChannelModelPrice.Create()`).
    async fn create_price(
        &self,
        ctx: &RequestContext,
        channel_id: i64,
        model_id: &str,
        currency_code: &str,
        price: &Value,
        reference_id: &str,
        now: &str,
    ) -> RepoResult<ChannelModelPriceRow> {
        guard_repo_principal(ctx)?;
        self.create_price_unchecked(
            ctx,
            channel_id,
            model_id,
            currency_code,
            price,
            reference_id,
            now,
        )
        .await
    }

    async fn update_price_unchecked(
        &self,
        ctx: &RequestContext,
        id: i64,
        currency_code: &str,
        price: &Value,
        reference_id: &str,
        now: &str,
    ) -> RepoResult<ChannelModelPriceRow>;

    /// Replace the price + reference id on an existing head row (Go
    /// `ChannelModelPrice.UpdateOneID(id).SetPrice(...).SetReferenceID(...)`).
    async fn update_price(
        &self,
        ctx: &RequestContext,
        id: i64,
        currency_code: &str,
        price: &Value,
        reference_id: &str,
        now: &str,
    ) -> RepoResult<ChannelModelPriceRow> {
        guard_repo_principal(ctx)?;
        self.update_price_unchecked(ctx, id, currency_code, price, reference_id, now)
            .await
    }

    async fn soft_delete_price_unchecked(
        &self,
        ctx: &RequestContext,
        id: i64,
        now: &str,
    ) -> RepoResult<()>;

    /// Soft-delete a head row. Go calls `ChannelModelPrice.DeleteOne`, which the
    /// SoftDeleteMixin intercepts into `SET deleted_at = now`.
    async fn soft_delete_price(&self, ctx: &RequestContext, id: i64, now: &str) -> RepoResult<()> {
        guard_repo_principal(ctx)?;
        self.soft_delete_price_unchecked(ctx, id, now).await
    }

    async fn archive_active_versions_unchecked(
        &self,
        ctx: &RequestContext,
        channel_model_price_id: i64,
        effective_end_at: &str,
    ) -> RepoResult<u64>;

    /// Flip every `active` version of a head row to `archived`, stamping
    /// `effective_end_at` (Go's pre-update / pre-delete version archival).
    /// Returns the affected-row count.
    async fn archive_active_versions(
        &self,
        ctx: &RequestContext,
        channel_model_price_id: i64,
        effective_end_at: &str,
    ) -> RepoResult<u64> {
        guard_repo_principal(ctx)?;
        self.archive_active_versions_unchecked(ctx, channel_model_price_id, effective_end_at)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_version_unchecked(
        &self,
        ctx: &RequestContext,
        channel_id: i64,
        model_id: &str,
        channel_model_price_id: i64,
        currency_code: &str,
        price: &Value,
        status: &str,
        effective_start_at: &str,
        reference_id: &str,
        now: &str,
    ) -> RepoResult<ChannelModelPriceVersionRow>;

    /// Append an immutable version row (Go `ChannelModelPriceVersion.Create()`).
    #[allow(clippy::too_many_arguments)]
    async fn create_version(
        &self,
        ctx: &RequestContext,
        channel_id: i64,
        model_id: &str,
        channel_model_price_id: i64,
        currency_code: &str,
        price: &Value,
        status: &str,
        effective_start_at: &str,
        reference_id: &str,
        now: &str,
    ) -> RepoResult<ChannelModelPriceVersionRow> {
        guard_repo_principal(ctx)?;
        self.create_version_unchecked(
            ctx,
            channel_id,
            model_id,
            channel_model_price_id,
            currency_code,
            price,
            status,
            effective_start_at,
            reference_id,
            now,
        )
        .await
    }
}
