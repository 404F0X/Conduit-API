//! DB-backed channel override template adapter (P-54).

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use conduit_admin_graphql::channel::{
    Channel, OrderDirection, OverrideMatch, OverrideOperation, OverrideOperationInput,
};
use conduit_admin_graphql::channel_override_template_ext as gql;
use conduit_admin_graphql::node::parse_guid;
use conduit_admin_graphql::pagination::{PageInfo, decode_offset_cursor, encode_offset_cursor};
use conduit_admin_graphql::scalars::{CursorScalar, TimeScalar};
use conduit_admin_graphql::system_settings_ext::HeaderEntry;
use conduit_core::objects::channel_settings::ChannelSettings;
use conduit_core::objects::overrides as core_override;
use conduit_db::repo::channel_override_template_repo::{
    ChannelOverrideTemplateRepo, CreateChannelOverrideTemplateInput as RepoCreateInput,
    UpdateChannelOverrideTemplateInput as RepoUpdateInput,
};
use conduit_db::repo::channel_repo::ChannelRepo;
use conduit_db::row::ChannelOverrideTemplateRow;
use conduit_db::{PolicyContext, Principal, RequestContext};

type ExtResult<T> = Result<T, gql::ChannelOverrideTemplateExtError>;

pub struct ChannelOverrideTemplateAdapter {
    templates: Arc<dyn ChannelOverrideTemplateRepo>,
    channels: Arc<dyn ChannelRepo>,
}

impl ChannelOverrideTemplateAdapter {
    pub fn new(
        templates: Arc<dyn ChannelOverrideTemplateRepo>,
        channels: Arc<dyn ChannelRepo>,
    ) -> Self {
        Self {
            templates,
            channels,
        }
    }

    fn error(err: impl std::fmt::Display) -> gql::ChannelOverrideTemplateExtError {
        gql::ChannelOverrideTemplateExtError::Operation(err.to_string())
    }

    async fn row(&self, id: &str, user_id: i64) -> ExtResult<ChannelOverrideTemplateRow> {
        let id = decode_id(id)?;
        self.templates
            .find(id, user_id)
            .await
            .map_err(Self::error)?
            .ok_or_else(|| Self::error("channel override template not found"))
    }

    async fn updated_channels(&self, ids: &[i64]) -> ExtResult<Vec<Channel>> {
        let ctx = RequestContext::new(PolicyContext::new(Principal::system()));
        let mut channels = Vec::with_capacity(ids.len());
        for id in ids {
            let row = self
                .channels
                .find_channel(&ctx, &id.to_string())
                .await
                .map_err(Self::error)?
                .ok_or_else(|| Self::error(format!("channel {id} not found")))?;
            channels.push(crate::conv::channel_row_to_gql(row));
        }
        Ok(channels)
    }
}

#[async_trait]
impl gql::ChannelOverrideTemplateExtServices for ChannelOverrideTemplateAdapter {
    async fn list(
        &self,
        user_id: i64,
        args: gql::ChannelOverrideTemplateConnectionArgs,
    ) -> ExtResult<gql::ChannelOverrideTemplateConnection> {
        validate_page_args(args.first, args.last)?;
        let mut rows = self.templates.list(user_id).await.map_err(Self::error)?;
        if let Some(filter) = &args.where_filter {
            rows.retain(|row| template_matches(row, filter));
        }
        if let Some(order) = &args.order_by {
            rows.sort_by(|left, right| {
                let ordering = match order.field {
                    gql::ChannelOverrideTemplateOrderField::CreatedAt => {
                        left.created_at.cmp(&right.created_at)
                    }
                    gql::ChannelOverrideTemplateOrderField::UpdatedAt => {
                        left.updated_at.cmp(&right.updated_at)
                    }
                };
                if order.direction == OrderDirection::Desc {
                    ordering.reverse()
                } else {
                    ordering
                }
            });
        }
        paginate(rows, args)
    }

    async fn create(
        &self,
        user_id: i64,
        input: gql::CreateChannelOverrideTemplateInput,
    ) -> ExtResult<gql::ChannelOverrideTemplate> {
        let header_ops = convert_ops(input.header_override_operations.unwrap_or_default());
        let body_ops = convert_ops(input.body_override_operations.unwrap_or_default());
        validate_header_ops(&header_ops)?;
        validate_body_ops(&body_ops)?;
        let row = self
            .templates
            .create(RepoCreateInput {
                user_id,
                name: input.name,
                description: input.description,
                override_parameters: "{}".to_string(),
                override_headers: "[]".to_string(),
                header_override_operations: json(&header_ops)?,
                body_override_operations: json(&body_ops)?,
            })
            .await
            .map_err(Self::error)?;
        row_to_gql(row)
    }

    async fn update(
        &self,
        user_id: i64,
        id: String,
        input: gql::UpdateChannelOverrideTemplateInput,
    ) -> ExtResult<gql::ChannelOverrideTemplate> {
        let id = decode_id(&id)?;
        let current = self
            .templates
            .find(id, user_id)
            .await
            .map_err(Self::error)?
            .ok_or_else(|| Self::error("channel override template not found"))?;

        let header_ops = merged_update_ops(
            parse_ops_value(current.header_override_operations.clone())?,
            input.header_override_operations,
            input.append_header_override_operations,
            input.clear_header_override_operations.unwrap_or(false),
        );
        let body_ops = merged_update_ops(
            parse_ops_value(current.body_override_operations.clone())?,
            input.body_override_operations,
            input.append_body_override_operations,
            input.clear_body_override_operations.unwrap_or(false),
        );
        if let Some(ops) = &header_ops {
            validate_header_ops(ops)?;
        }
        if let Some(ops) = &body_ops {
            validate_body_ops(ops)?;
        }

        let row = self
            .templates
            .update(
                id,
                user_id,
                RepoUpdateInput {
                    name: input.name,
                    description: input.description,
                    clear_description: input.clear_description.unwrap_or(false),
                    header_override_operations: header_ops.as_ref().map(json).transpose()?,
                    body_override_operations: body_ops.as_ref().map(json).transpose()?,
                    ..Default::default()
                },
            )
            .await
            .map_err(Self::error)?;
        row_to_gql(row)
    }

    async fn delete(&self, user_id: i64, id: String) -> ExtResult<bool> {
        self.templates
            .soft_delete(decode_id(&id)?, user_id)
            .await
            .map_err(Self::error)?;
        Ok(true)
    }

    async fn apply(
        &self,
        user_id: i64,
        input: gql::ApplyChannelOverrideTemplateInput,
    ) -> ExtResult<gql::ApplyChannelOverrideTemplatePayload> {
        let template = self.row(input.template_id.as_str(), user_id).await?;
        let channel_ids = decode_channel_ids(&input.channel_ids)?;
        let template_headers = parse_ops_value(template.header_override_operations)?;
        let template_body = parse_ops_value(template.body_override_operations)?;
        let mut updates = Vec::with_capacity(channel_ids.len());

        for channel_id in &channel_ids {
            let raw = self
                .templates
                .channel_settings(*channel_id)
                .await
                .map_err(Self::error)?
                .ok_or_else(|| Self::error(format!("channel {channel_id} not found")))?;
            let mut settings: ChannelSettings = if raw.trim().is_empty() {
                ChannelSettings::default()
            } else {
                serde_json::from_str(&raw).map_err(Self::error)?
            };
            let replace = input.mode == Some(gql::OverrideApplyMode::Replace);
            let existing_headers = existing_header_ops(&settings);
            let existing_body = existing_body_ops(&settings)?;
            settings.header_override_operations = if replace {
                template_headers.clone()
            } else {
                merge_header_ops(existing_headers, &template_headers)
            };
            settings.body_override_operations = if replace {
                template_body.clone()
            } else {
                merge_body_ops(existing_body, &template_body)
            };
            settings.override_headers.clear();
            settings.override_parameters.clear();
            updates.push((*channel_id, json(&settings)?));
        }
        self.templates
            .set_channel_settings_batch(&updates)
            .await
            .map_err(Self::error)?;
        let channels = self.updated_channels(&channel_ids).await?;
        Ok(gql::ApplyChannelOverrideTemplatePayload {
            success: true,
            updated: channels.len() as i32,
            channels,
        })
    }

    async fn clear(
        &self,
        input: gql::ClearChannelOverrideTemplatesInput,
    ) -> ExtResult<gql::ClearChannelOverrideTemplatesPayload> {
        let channel_ids = decode_channel_ids(&input.channel_ids)?;
        let mut updates = Vec::with_capacity(channel_ids.len());
        for channel_id in &channel_ids {
            let raw = self
                .templates
                .channel_settings(*channel_id)
                .await
                .map_err(Self::error)?
                .ok_or_else(|| Self::error(format!("channel {channel_id} not found")))?;
            let mut settings: ChannelSettings = if raw.trim().is_empty() {
                ChannelSettings::default()
            } else {
                serde_json::from_str(&raw).map_err(Self::error)?
            };
            settings.header_override_operations.clear();
            settings.body_override_operations.clear();
            settings.override_headers.clear();
            settings.override_parameters.clear();
            updates.push((*channel_id, json(&settings)?));
        }
        self.templates
            .set_channel_settings_batch(&updates)
            .await
            .map_err(Self::error)?;
        let channels = self.updated_channels(&channel_ids).await?;
        Ok(gql::ClearChannelOverrideTemplatesPayload {
            success: true,
            updated: channels.len() as i32,
            channels,
        })
    }
}

fn decode_id(raw: &str) -> ExtResult<i64> {
    if let Ok(guid) = parse_guid(raw) {
        return Ok(guid.id);
    }
    raw.parse::<i64>()
        .map_err(ChannelOverrideTemplateAdapter::error)
}

fn decode_channel_ids(ids: &[async_graphql::ID]) -> ExtResult<Vec<i64>> {
    if ids.is_empty() {
        return Err(ChannelOverrideTemplateAdapter::error(
            "at least one channel is required",
        ));
    }
    let decoded = ids
        .iter()
        .map(|id| decode_id(id.as_str()))
        .collect::<ExtResult<Vec<_>>>()?;
    let unique = decoded.iter().copied().collect::<HashSet<_>>();
    if unique.len() != decoded.len() {
        return Err(ChannelOverrideTemplateAdapter::error(
            "duplicate channel IDs are not allowed",
        ));
    }
    Ok(decoded)
}

fn convert_ops(inputs: Vec<OverrideOperationInput>) -> Vec<core_override::OverrideOperation> {
    inputs
        .into_iter()
        .map(|input| core_override::OverrideOperation {
            op: input.op,
            path: input.path.unwrap_or_default(),
            from: input.from.unwrap_or_default(),
            to: input.to.unwrap_or_default(),
            value: input.value.unwrap_or_default(),
            condition: input.condition.unwrap_or_default(),
            r#match: input.match_.map(|m| core_override::OverrideMatch {
                path: m.path,
                eq: m.eq,
            }),
            index: input.index,
            splat: input.splat,
        })
        .collect()
}

fn merged_update_ops(
    current: Vec<core_override::OverrideOperation>,
    replacement: Option<Vec<OverrideOperationInput>>,
    append: Option<Vec<OverrideOperationInput>>,
    clear: bool,
) -> Option<Vec<core_override::OverrideOperation>> {
    if !clear && replacement.is_none() && append.is_none() {
        return None;
    }
    let mut result = if clear {
        Vec::new()
    } else if let Some(replacement) = replacement {
        convert_ops(replacement)
    } else {
        current
    };
    if let Some(append) = append {
        result.extend(convert_ops(append));
    }
    Some(result)
}

fn validate_header_ops(ops: &[core_override::OverrideOperation]) -> ExtResult<()> {
    for (index, op) in ops.iter().enumerate() {
        match op.op.as_str() {
            "set" | "delete" if op.path.trim().is_empty() => {
                return Err(ChannelOverrideTemplateAdapter::error(format!(
                    "header operation at index {index} has an empty path"
                )));
            }
            "rename" | "copy" if op.from.trim().is_empty() || op.to.trim().is_empty() => {
                return Err(ChannelOverrideTemplateAdapter::error(format!(
                    "header operation at index {index} requires non-empty from and to"
                )));
            }
            "set" | "delete" | "rename" | "copy" => {}
            other => {
                return Err(ChannelOverrideTemplateAdapter::error(format!(
                    "header operation at index {index} has unknown op {other:?}"
                )));
            }
        }
    }
    Ok(())
}

fn validate_body_ops(ops: &[core_override::OverrideOperation]) -> ExtResult<()> {
    for (index, op) in ops.iter().enumerate() {
        match op.op.as_str() {
            "set" | "delete" | "array_append" | "array_prepend" | "array_insert"
            | "array_remove"
                if op.path.trim().is_empty() =>
            {
                return Err(ChannelOverrideTemplateAdapter::error(format!(
                    "body operation at index {index} has an empty path"
                )));
            }
            "set" | "array_append" | "array_prepend" | "array_insert"
                if op.path.eq_ignore_ascii_case("stream") =>
            {
                return Err(ChannelOverrideTemplateAdapter::error(
                    "override parameters cannot contain the field \"stream\"",
                ));
            }
            "rename" | "copy" if op.from.trim().is_empty() || op.to.trim().is_empty() => {
                return Err(ChannelOverrideTemplateAdapter::error(format!(
                    "body operation at index {index} requires non-empty from and to"
                )));
            }
            "array_insert" if op.index.is_none() => {
                return Err(ChannelOverrideTemplateAdapter::error(format!(
                    "body operation at index {index} requires an index"
                )));
            }
            "array_remove"
                if op
                    .r#match
                    .as_ref()
                    .is_none_or(|m| m.path.trim().is_empty() || m.eq.trim().is_empty()) =>
            {
                return Err(ChannelOverrideTemplateAdapter::error(format!(
                    "body operation at index {index} requires a complete match"
                )));
            }
            "set" | "delete" | "rename" | "copy" | "array_append" | "array_prepend"
            | "array_insert" | "array_remove" => {}
            other => {
                return Err(ChannelOverrideTemplateAdapter::error(format!(
                    "body operation at index {index} has unknown op {other:?}"
                )));
            }
        }
    }
    Ok(())
}

fn parse_ops_value(value: serde_json::Value) -> ExtResult<Vec<core_override::OverrideOperation>> {
    serde_json::from_value(value).map_err(ChannelOverrideTemplateAdapter::error)
}

fn json<T: serde::Serialize>(value: &T) -> ExtResult<String> {
    serde_json::to_string(value).map_err(ChannelOverrideTemplateAdapter::error)
}

fn existing_header_ops(settings: &ChannelSettings) -> Vec<core_override::OverrideOperation> {
    if !settings.header_override_operations.is_empty() {
        return settings.header_override_operations.clone();
    }
    core_override::header_entries_to_override_operations(&settings.override_headers)
        .unwrap_or_default()
}

fn existing_body_ops(
    settings: &ChannelSettings,
) -> ExtResult<Vec<core_override::OverrideOperation>> {
    if !settings.body_override_operations.is_empty() {
        return Ok(settings.body_override_operations.clone());
    }
    core_override::parse_override_operations(&settings.override_parameters)
        .map(|ops| ops.unwrap_or_default())
        .map_err(ChannelOverrideTemplateAdapter::error)
}

fn merge_header_ops(
    mut existing: Vec<core_override::OverrideOperation>,
    template: &[core_override::OverrideOperation],
) -> Vec<core_override::OverrideOperation> {
    for op in template {
        if matches!(op.op.as_str(), "rename" | "copy") {
            existing.push(op.clone());
            continue;
        }
        if let Some(index) = existing.iter().position(|item| {
            matches!(item.op.as_str(), "set" | "delete") && item.path.eq_ignore_ascii_case(&op.path)
        }) {
            existing[index] = op.clone();
        } else {
            existing.push(op.clone());
        }
    }
    existing
}

fn merge_body_ops(
    mut existing: Vec<core_override::OverrideOperation>,
    template: &[core_override::OverrideOperation],
) -> Vec<core_override::OverrideOperation> {
    for op in template {
        if matches!(
            op.op.as_str(),
            "rename" | "copy" | "array_append" | "array_prepend" | "array_insert"
        ) {
            existing.push(op.clone());
            continue;
        }
        if let Some(index) = existing
            .iter()
            .position(|item| matches!(item.op.as_str(), "set" | "delete") && item.path == op.path)
        {
            existing[index] = op.clone();
        } else {
            existing.push(op.clone());
        }
    }
    existing
}

fn template_matches(
    row: &ChannelOverrideTemplateRow,
    filter: &gql::ChannelOverrideTemplateWhereInput,
) -> bool {
    if let Some(name) = &filter.name
        && row.name != *name
    {
        return false;
    }
    if let Some(needle) = &filter.name_contains
        && !row.name.contains(needle)
    {
        return false;
    }
    if let Some(needle) = &filter.name_contains_fold
        && !row.name.to_lowercase().contains(&needle.to_lowercase())
    {
        return false;
    }
    true
}

fn validate_page_args(first: Option<i32>, last: Option<i32>) -> ExtResult<()> {
    if first.is_some() && last.is_some() {
        return Err(ChannelOverrideTemplateAdapter::error(
            "first and last cannot be used together",
        ));
    }
    if first.is_some_and(|value| value < 0) || last.is_some_and(|value| value < 0) {
        return Err(ChannelOverrideTemplateAdapter::error(
            "pagination size must not be negative",
        ));
    }
    Ok(())
}

fn paginate(
    rows: Vec<ChannelOverrideTemplateRow>,
    args: gql::ChannelOverrideTemplateConnectionArgs,
) -> ExtResult<gql::ChannelOverrideTemplateConnection> {
    let total = rows.len();
    let mut start = args
        .after
        .as_deref()
        .map(decode_offset_cursor)
        .transpose()
        .map_err(|_| ChannelOverrideTemplateAdapter::error("invalid after cursor"))?
        .map_or(0, |offset| offset.saturating_add(1) as usize)
        .min(total);
    let mut end = args
        .before
        .as_deref()
        .map(decode_offset_cursor)
        .transpose()
        .map_err(|_| ChannelOverrideTemplateAdapter::error("invalid before cursor"))?
        .map_or(total, |offset| offset as usize)
        .min(total);
    if end < start {
        end = start;
    }
    if let Some(first) = args.first {
        end = end.min(start.saturating_add(first as usize));
    }
    if let Some(last) = args.last {
        start = start.max(end.saturating_sub(last as usize));
    }
    let mut edges = Vec::with_capacity(end.saturating_sub(start));
    for (index, row) in rows.into_iter().enumerate().take(end).skip(start) {
        edges.push(Some(gql::ChannelOverrideTemplateEdge {
            node: Some(row_to_gql(row)?),
            cursor: CursorScalar(encode_offset_cursor(index as u64)),
        }));
    }
    let start_cursor = edges
        .first()
        .and_then(|edge| edge.as_ref())
        .map(|edge| edge.cursor.clone());
    let end_cursor = edges
        .last()
        .and_then(|edge| edge.as_ref())
        .map(|edge| edge.cursor.clone());
    Ok(gql::ChannelOverrideTemplateConnection {
        edges: Some(edges),
        page_info: PageInfo {
            has_next_page: end < total,
            has_previous_page: start > 0,
            start_cursor,
            end_cursor,
        },
        total_count: total as i64,
    })
}

fn row_to_gql(row: ChannelOverrideTemplateRow) -> ExtResult<gql::ChannelOverrideTemplate> {
    let headers: Vec<conduit_core::objects::channel_settings::HeaderEntry> =
        serde_json::from_value(row.override_headers)
            .map_err(ChannelOverrideTemplateAdapter::error)?;
    Ok(gql::ChannelOverrideTemplate {
        id: async_graphql::ID::from(row.id),
        created_at: TimeScalar(row.created_at),
        updated_at: TimeScalar(row.updated_at),
        user_id: row.user_id.map(async_graphql::ID::from),
        name: row.name,
        description: row.description,
        override_parameters: row.override_parameters,
        override_headers: headers
            .into_iter()
            .map(|entry| HeaderEntry {
                key: entry.key,
                value: entry.value,
            })
            .collect(),
        header_override_operations: parse_ops_value(row.header_override_operations)?
            .into_iter()
            .map(core_op_to_gql)
            .collect(),
        body_override_operations: parse_ops_value(row.body_override_operations)?
            .into_iter()
            .map(core_op_to_gql)
            .collect(),
        // The edge is nullable in the Go contract. Loading the owner is kept
        // out of this hot list path until a shared user DataLoader is wired.
        user: None,
    })
}

fn core_op_to_gql(op: core_override::OverrideOperation) -> OverrideOperation {
    fn nonempty(value: String) -> Option<String> {
        (!value.is_empty()).then_some(value)
    }
    OverrideOperation {
        op: op.op,
        path: nonempty(op.path),
        from: nonempty(op.from),
        to: nonempty(op.to),
        value: nonempty(op.value),
        condition: nonempty(op.condition),
        match_: op.r#match.map(|m| OverrideMatch {
            path: m.path,
            eq: m.eq,
        }),
        index: op.index,
        splat: op.splat,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(kind: &str, path: &str, value: &str) -> core_override::OverrideOperation {
        core_override::OverrideOperation {
            op: kind.to_string(),
            path: path.to_string(),
            value: value.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn body_merge_replaces_matching_set_and_preserves_others() {
        let merged = merge_body_ops(
            vec![op("set", "temperature", "0.7"), op("set", "top_p", "0.9")],
            &[
                op("set", "temperature", "1.0"),
                op("set", "max_tokens", "10"),
            ],
        );
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].value, "1.0");
        assert!(merged.iter().any(|item| item.path == "top_p"));
        assert!(merged.iter().any(|item| item.path == "max_tokens"));
    }

    #[test]
    fn header_merge_matches_paths_case_insensitively() {
        let merged = merge_header_ops(
            vec![op("set", "Authorization", "old")],
            &[op("set", "authorization", "new")],
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].value, "new");
    }

    #[tokio::test]
    async fn postgres_template_repo_lifecycle_when_dsn_is_provided()
    -> Result<(), Box<dyn std::error::Error>> {
        let Ok(dsn) = std::env::var("CONDUIT_TEST_POSTGRES_DSN") else {
            return Ok(());
        };
        let database = crate::postgres_test_support::IsolatedPostgres::new(&dsn).await?;
        let repo = conduit_db::PgChannelOverrideTemplateRepo::new(database.pool.clone());
        let created = repo
            .create(RepoCreateInput {
                user_id: 17,
                name: "postgres-template".to_string(),
                description: Some("before".to_string()),
                override_parameters: "{}".to_string(),
                override_headers: "[]".to_string(),
                header_override_operations: "[]".to_string(),
                body_override_operations: r#"[{"op":"set","path":"temperature","value":"1"}]"#
                    .to_string(),
            })
            .await?;
        assert_eq!(repo.list(17).await?.len(), 1);
        let updated = repo
            .update(
                created.id.parse()?,
                17,
                RepoUpdateInput {
                    description: Some("after".to_string()),
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(updated.description.as_deref(), Some("after"));
        repo.soft_delete(created.id.parse()?, 17).await?;
        assert!(repo.list(17).await?.is_empty());
        database.cleanup().await?;
        Ok(())
    }
}
