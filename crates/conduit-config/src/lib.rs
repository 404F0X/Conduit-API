#![forbid(unsafe_code)]

//! Configuration model, loading, validation, and schema helpers for Conduit API.

pub mod export;
pub mod loader;
pub mod model;
pub mod schema;
pub mod validate;

pub use export::{
    EnvEntry, SECRET_MASK, default_env_entries, env_entries, masked_config_preview,
    render_default_env, render_env, render_masked_config_preview, render_masked_env,
};
pub use loader::{
    CliOverrides, ConfigError, discover_config_file, load_default_search, load_from_path,
};
pub use model::AppConfig;
pub use validate::{ValidationError, validate};
