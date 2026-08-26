use std::sync::Arc;

use async_graphql::{Context, ID, InputObject, SimpleObject};
use async_trait::async_trait;

#[derive(Debug, Clone, PartialEq, Eq, InputObject)]
#[graphql(name = "GetChannelProbeDataInput")]
pub struct GetChannelProbeDataInput {
    #[graphql(name = "channelIDs")]
    pub channel_ids: Vec<ID>,
}

#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "ChannelProbePoint")]
pub struct ChannelProbePoint {
    pub timestamp: i32,
    pub total_request_count: i32,
    pub success_request_count: i32,
    pub avg_tokens_per_second: Option<f64>,
    pub avg_time_to_first_token_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "ChannelProbeData")]
pub struct ChannelProbeData {
    #[graphql(name = "channelID")]
    pub channel_id: ID,
    pub points: Vec<ChannelProbePoint>,
}

#[derive(Debug, Clone, PartialEq, SimpleObject)]
#[graphql(name = "PublicChannelHealth")]
pub struct PublicChannelHealth {
    pub status: String,
    pub success_rate: Option<f64>,
    pub avg_time_to_first_token_ms: Option<f64>,
    pub avg_tokens_per_second: Option<f64>,
    pub last_updated_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, SimpleObject)]
#[graphql(name = "PublicChannelHealthSettings")]
pub struct PublicChannelHealthSettings {
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, InputObject)]
#[graphql(name = "UpdatePublicChannelHealthSettingsInput")]
pub struct UpdatePublicChannelHealthSettingsInput {
    pub enabled: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ChannelProbeError {
    #[error("channel probe service is not available")]
    Unavailable,
    #[error("invalid channel id: {0}")]
    InvalidChannelId(String),
    #[error("channel probe query failed: {0}")]
    Query(String),
}

#[async_trait]
pub trait ChannelProbeServices: Send + Sync {
    async fn channel_probe_data(
        &self,
        input: GetChannelProbeDataInput,
    ) -> Result<Vec<ChannelProbeData>, ChannelProbeError>;

    async fn public_channel_health(&self)
    -> Result<Option<PublicChannelHealth>, ChannelProbeError>;

    async fn public_channel_health_settings(
        &self,
    ) -> Result<PublicChannelHealthSettings, ChannelProbeError>;

    async fn set_public_channel_health_settings(
        &self,
        enabled: bool,
    ) -> Result<(), ChannelProbeError>;
}

pub fn channel_probe_services<'a>(
    ctx: &'a Context<'_>,
) -> Result<&'a Arc<dyn ChannelProbeServices>, ChannelProbeError> {
    ctx.data_opt::<Arc<dyn ChannelProbeServices>>()
        .ok_or(ChannelProbeError::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::{EmptySubscription, Object, Schema};

    struct Fake;

    #[async_trait]
    impl ChannelProbeServices for Fake {
        async fn channel_probe_data(
            &self,
            input: GetChannelProbeDataInput,
        ) -> Result<Vec<ChannelProbeData>, ChannelProbeError> {
            Ok(input
                .channel_ids
                .into_iter()
                .map(|channel_id| ChannelProbeData {
                    channel_id,
                    points: vec![ChannelProbePoint {
                        timestamp: 1_700_000_000,
                        total_request_count: 3,
                        success_request_count: 2,
                        avg_tokens_per_second: Some(12.5),
                        avg_time_to_first_token_ms: None,
                    }],
                })
                .collect())
        }

        async fn public_channel_health(
            &self,
        ) -> Result<Option<PublicChannelHealth>, ChannelProbeError> {
            Ok(None)
        }

        async fn public_channel_health_settings(
            &self,
        ) -> Result<PublicChannelHealthSettings, ChannelProbeError> {
            Ok(PublicChannelHealthSettings { enabled: false })
        }

        async fn set_public_channel_health_settings(
            &self,
            _enabled: bool,
        ) -> Result<(), ChannelProbeError> {
            Ok(())
        }
    }

    struct Query;

    #[Object]
    impl Query {
        async fn channel_probe_data(
            &self,
            ctx: &Context<'_>,
            input: GetChannelProbeDataInput,
        ) -> Result<Vec<ChannelProbeData>, String> {
            channel_probe_services(ctx)
                .map_err(|error| error.to_string())?
                .channel_probe_data(input)
                .await
                .map_err(|error| error.to_string())
        }
    }

    #[tokio::test]
    async fn query_matches_frontend_shape() -> Result<(), Box<dyn std::error::Error>> {
        let service: Arc<dyn ChannelProbeServices> = Arc::new(Fake);
        let schema = Schema::build(Query, async_graphql::EmptyMutation, EmptySubscription)
            .data(service)
            .finish();
        let response = schema
            .execute(
                r#"query {
                    channelProbeData(input: { channelIDs: ["gid://conduit/Channel/7"] }) {
                        channelID
                        points { timestamp totalRequestCount successRequestCount avgTokensPerSecond avgTimeToFirstTokenMs }
                    }
                }"#,
            )
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let json = response.data.into_json()?;
        assert_eq!(
            json["channelProbeData"][0]["channelID"],
            "gid://conduit/Channel/7"
        );
        assert_eq!(
            json["channelProbeData"][0]["points"][0]["totalRequestCount"],
            3
        );
        Ok(())
    }
}
