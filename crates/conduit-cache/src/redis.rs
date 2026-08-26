use crate::{Cache, CacheError, CacheResult};
use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedisMode {
    Standalone,
    Sentinel,
    Cluster,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisConnectionConfig {
    pub url: Option<String>,
    pub addr: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub db: u8,
    pub tls: bool,
    pub mode: RedisMode,
    pub addrs: Vec<String>,
    pub master_name: Option<String>,
}

impl RedisConnectionConfig {
    pub fn standalone(addr: impl Into<String>) -> Self {
        Self {
            url: None,
            addr: addr.into(),
            username: None,
            password: None,
            db: 0,
            tls: false,
            mode: RedisMode::Standalone,
            addrs: Vec::new(),
            master_name: None,
        }
    }

    pub fn from_url(input: &str) -> CacheResult<Self> {
        let parsed = Url::parse(input)
            .map_err(|err| CacheError::InvalidConfig(format!("invalid Redis URL: {err}")))?;
        let tls = match parsed.scheme() {
            "redis" => false,
            "rediss" => true,
            other => {
                return Err(CacheError::InvalidConfig(format!(
                    "unsupported Redis URL scheme {other:?}"
                )));
            }
        };

        let host = parsed.host_str().ok_or_else(|| {
            CacheError::InvalidConfig("Redis URL must include a host".to_string())
        })?;
        let port = parsed.port().unwrap_or(6379);
        let db_path = parsed.path().trim_start_matches('/');
        let db = if db_path.is_empty() {
            0
        } else {
            db_path.parse::<u8>().map_err(|err| {
                CacheError::InvalidConfig(format!(
                    "invalid Redis database index {db_path:?}: {err}"
                ))
            })?
        };
        let username = if parsed.username().is_empty() {
            None
        } else {
            Some(parsed.username().to_string())
        };
        let password = parsed.password().map(ToString::to_string);

        Ok(Self {
            url: Some(input.to_string()),
            addr: format!("{host}:{port}"),
            username,
            password,
            db,
            tls,
            mode: RedisMode::Standalone,
            addrs: Vec::new(),
            master_name: None,
        })
    }

    pub fn validate_supported(&self) -> CacheResult<()> {
        match self.mode {
            RedisMode::Standalone => {
                if self.addr.trim().is_empty() && self.url.is_none() {
                    Err(CacheError::InvalidConfig(
                        "Redis address is empty".to_string(),
                    ))
                } else {
                    Ok(())
                }
            }
            RedisMode::Sentinel => {
                if self.master_name.as_deref().unwrap_or_default().is_empty() {
                    return Err(CacheError::InvalidConfig(
                        "Redis sentinel master_name is required".to_string(),
                    ));
                }
                if self.addrs.is_empty() && self.addr.trim().is_empty() {
                    return Err(CacheError::InvalidConfig(
                        "Redis sentinel address list is empty".to_string(),
                    ));
                }
                Ok(())
            }
            RedisMode::Cluster => {
                if self.db != 0 {
                    return Err(CacheError::InvalidConfig(
                        "Redis cluster supports database 0 only".to_string(),
                    ));
                }
                if self.addrs.is_empty() && self.addr.trim().is_empty() {
                    Err(CacheError::InvalidConfig(
                        "Redis cluster address list is empty".to_string(),
                    ))
                } else {
                    Ok(())
                }
            }
        }
    }
}

impl Default for RedisConnectionConfig {
    fn default() -> Self {
        Self::standalone("127.0.0.1:6379")
    }
}

#[derive(Debug, Clone)]
pub struct RedisCache {
    config: RedisConnectionConfig,
}

impl RedisCache {
    pub fn new(config: RedisConnectionConfig) -> CacheResult<Self> {
        config.validate_supported()?;
        #[cfg(not(feature = "redis"))]
        return Err(CacheError::Unsupported(
            "Redis feature is disabled at compile time".to_string(),
        ));
        #[cfg(feature = "redis")]
        Ok(Self { config })
    }

    pub fn from_url(url: &str) -> CacheResult<Self> {
        Self::new(RedisConnectionConfig::from_url(url)?)
    }

    pub const fn config(&self) -> &RedisConnectionConfig {
        &self.config
    }

    #[cfg(feature = "redis")]
    fn url_for_addr_with_db(&self, addr: &str, db: u8) -> CacheResult<String> {
        let scheme = if self.config.tls { "rediss" } else { "redis" };
        let mut url = Url::parse(&format!("{scheme}://{addr}")).map_err(|error| {
            CacheError::InvalidConfig(format!("invalid Redis address: {error}"))
        })?;
        if let Some(username) = self.config.username.as_deref() {
            url.set_username(username)
                .map_err(|_| CacheError::InvalidConfig("invalid Redis username".to_string()))?;
        }
        if let Some(password) = self.config.password.as_deref() {
            url.set_password(Some(password))
                .map_err(|_| CacheError::InvalidConfig("invalid Redis password".to_string()))?;
        }
        url.set_path(&format!("/{db}"));
        Ok(url.to_string())
    }

    #[cfg(feature = "redis")]
    fn url_for_addr(&self, addr: &str) -> CacheResult<String> {
        self.url_for_addr_with_db(addr, self.config.db)
    }

    #[cfg(feature = "redis")]
    async fn standalone_connection(
        &self,
        addr: &str,
    ) -> CacheResult<redis::aio::MultiplexedConnection> {
        let url = if self.config.mode == RedisMode::Standalone {
            self.config.url.clone().unwrap_or(self.url_for_addr(addr)?)
        } else {
            self.url_for_addr(addr)?
        };
        let client =
            redis::Client::open(url).map_err(|error| CacheError::Unavailable(error.to_string()))?;
        client
            .get_multiplexed_async_connection()
            .await
            .map_err(|error| CacheError::Unavailable(error.to_string()))
    }

    #[cfg(feature = "redis")]
    async fn sentinel_master_addr(&self) -> CacheResult<String> {
        let master_name = self.config.master_name.as_deref().ok_or_else(|| {
            CacheError::InvalidConfig("Redis sentinel master_name is required".to_string())
        })?;
        let addrs = if self.config.addrs.is_empty() {
            vec![self.config.addr.clone()]
        } else {
            self.config.addrs.clone()
        };
        let mut last_error = None;
        for addr in addrs {
            let sentinel_url = self.url_for_addr_with_db(&addr, 0)?;
            let sentinel = redis::Client::open(sentinel_url)
                .map_err(|error| CacheError::Unavailable(error.to_string()))?;
            match sentinel
                .get_multiplexed_async_connection()
                .await
                .map_err(|error| CacheError::Unavailable(error.to_string()))
            {
                Ok(mut connection) => {
                    let result: redis::RedisResult<Option<(String, u16)>> = redis::cmd("SENTINEL")
                        .arg("get-master-addr-by-name")
                        .arg(master_name)
                        .query_async(&mut connection)
                        .await;
                    match result {
                        Ok(Some((host, port))) => return Ok(format!("{host}:{port}")),
                        Ok(None) => last_error = Some("master not found".to_string()),
                        Err(error) => last_error = Some(error.to_string()),
                    }
                }
                Err(error) => last_error = Some(error.to_string()),
            }
        }
        Err(CacheError::Unavailable(format!(
            "Redis sentinel discovery failed: {}",
            last_error.unwrap_or_else(|| "no sentinel responded".to_string())
        )))
    }

    #[cfg(feature = "redis")]
    async fn query<T: redis::FromRedisValue>(&self, command: &redis::Cmd) -> CacheResult<T> {
        match self.config.mode {
            RedisMode::Standalone => {
                let mut connection = self.standalone_connection(&self.config.addr).await?;
                command
                    .query_async(&mut connection)
                    .await
                    .map_err(|error| CacheError::Unavailable(error.to_string()))
            }
            RedisMode::Sentinel => {
                let master = self.sentinel_master_addr().await?;
                let mut connection = self.standalone_connection(&master).await?;
                command
                    .query_async(&mut connection)
                    .await
                    .map_err(|error| CacheError::Unavailable(error.to_string()))
            }
            RedisMode::Cluster => {
                let addrs = if self.config.addrs.is_empty() {
                    vec![self.config.addr.clone()]
                } else {
                    self.config.addrs.clone()
                };
                let nodes = addrs
                    .iter()
                    .map(|addr| self.url_for_addr(addr))
                    .collect::<CacheResult<Vec<_>>>()?;
                let client = redis::cluster::ClusterClient::new(nodes)
                    .map_err(|error| CacheError::Unavailable(error.to_string()))?;
                let mut connection = client
                    .get_async_connection()
                    .await
                    .map_err(|error| CacheError::Unavailable(error.to_string()))?;
                command
                    .query_async(&mut connection)
                    .await
                    .map_err(|error| CacheError::Unavailable(error.to_string()))
            }
        }
    }
}

#[async_trait]
impl Cache for RedisCache {
    async fn get(&self, key: &str) -> CacheResult<Option<Value>> {
        #[cfg(feature = "redis")]
        {
            let raw: Option<String> = self.query(redis::cmd("GET").arg(key)).await?;
            return raw
                .map(|value| {
                    serde_json::from_str(&value)
                        .map_err(|e| CacheError::Serialization(e.to_string()))
                })
                .transpose();
        }
        #[cfg(not(feature = "redis"))]
        {
            let _ = key;
            Err(CacheError::Unsupported(
                "Redis feature is disabled".to_string(),
            ))
        }
    }

    async fn set(&self, key: &str, value: Value, ttl: Option<Duration>) -> CacheResult<()> {
        #[cfg(feature = "redis")]
        {
            let raw = serde_json::to_string(&value)
                .map_err(|error| CacheError::Serialization(error.to_string()))?;
            let mut command = redis::cmd("SET");
            command.arg(key).arg(raw);
            if let Some(ttl) = ttl {
                command.arg("PX").arg(ttl.as_millis().max(1) as u64);
            }
            let _: String = self.query(&command).await?;
            return Ok(());
        }
        #[cfg(not(feature = "redis"))]
        {
            let _ = (key, value, ttl);
            Err(CacheError::Unsupported(
                "Redis feature is disabled".to_string(),
            ))
        }
    }

    async fn delete(&self, key: &str) -> CacheResult<()> {
        #[cfg(feature = "redis")]
        {
            let _: u64 = self.query(redis::cmd("DEL").arg(key)).await?;
            return Ok(());
        }
        #[cfg(not(feature = "redis"))]
        {
            let _ = key;
            Err(CacheError::Unsupported(
                "Redis feature is disabled".to_string(),
            ))
        }
    }

    async fn invalidate_prefix(&self, prefix: &str) -> CacheResult<()> {
        #[cfg(feature = "redis")]
        {
            let keys: Vec<String> = self
                .query(redis::cmd("KEYS").arg(format!("{prefix}*")))
                .await?;
            if !keys.is_empty() {
                let mut command = redis::cmd("DEL");
                command.arg(keys);
                let _: u64 = self.query(&command).await?;
            }
            return Ok(());
        }
        #[cfg(not(feature = "redis"))]
        {
            let _ = prefix;
            Err(CacheError::Unsupported(
                "Redis feature is disabled".to_string(),
            ))
        }
    }

    async fn clear(&self) -> CacheResult<()> {
        #[cfg(feature = "redis")]
        {
            let _: String = self.query(&redis::cmd("FLUSHDB")).await?;
            return Ok(());
        }
        #[cfg(not(feature = "redis"))]
        Err(CacheError::Unsupported(
            "Redis feature is disabled".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_redis_url_with_auth_and_db() -> CacheResult<()> {
        let config = RedisConnectionConfig::from_url("redis://user:secret@localhost:6380/2")?;

        assert_eq!(config.addr, "localhost:6380");
        assert_eq!(config.username, Some("user".to_string()));
        assert_eq!(config.password, Some("secret".to_string()));
        assert_eq!(config.db, 2);
        assert!(!config.tls);
        assert_eq!(config.mode, RedisMode::Standalone);

        Ok(())
    }

    #[test]
    fn parses_rediss_url_as_tls() -> CacheResult<()> {
        let config = RedisConnectionConfig::from_url("rediss://cache.example.test/0")?;

        assert_eq!(config.addr, "cache.example.test:6379");
        assert_eq!(config.db, 0);
        assert!(config.tls);

        Ok(())
    }

    #[test]
    fn rejects_unsupported_url_scheme() {
        let err = RedisConnectionConfig::from_url("http://localhost:6379")
            .err()
            .map(|err| err.to_string());

        assert_eq!(
            err,
            Some("invalid cache config: unsupported Redis URL scheme \"http\"".to_string())
        );
    }

    #[test]
    fn rejects_invalid_database_index() {
        let err = RedisConnectionConfig::from_url("redis://localhost/not-a-db")
            .err()
            .map(|err| err.to_string());

        assert_eq!(
            err,
            Some(
                "invalid cache config: invalid Redis database index \"not-a-db\": invalid digit found in string"
                    .to_string()
            )
        );
    }

    #[test]
    fn accepts_valid_sentinel_and_cluster_modes() -> CacheResult<()> {
        let mut sentinel = RedisConnectionConfig::standalone("localhost:6379");
        sentinel.mode = RedisMode::Sentinel;
        sentinel.master_name = Some("mymaster".to_string());
        sentinel.validate_supported()?;

        let mut cluster = RedisConnectionConfig::standalone("localhost:6379");
        cluster.mode = RedisMode::Cluster;
        cluster.addrs = vec!["localhost:6379".to_string(), "localhost:6380".to_string()];
        cluster.validate_supported()?;
        Ok(())
    }

    #[test]
    fn rejects_invalid_sentinel_and_cluster_config() {
        let mut sentinel = RedisConnectionConfig::standalone("localhost:6379");
        sentinel.mode = RedisMode::Sentinel;
        assert!(matches!(
            sentinel.validate_supported(),
            Err(CacheError::InvalidConfig(_))
        ));

        let mut cluster = RedisConnectionConfig::standalone("localhost:6379");
        cluster.mode = RedisMode::Cluster;
        cluster.db = 2;
        assert!(matches!(
            cluster.validate_supported(),
            Err(CacheError::InvalidConfig(_))
        ));
    }
}
