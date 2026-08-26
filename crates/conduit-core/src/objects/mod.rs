//! JSON domain objects ported from the Go `internal/objects` package.
//!
//! Each submodule ports a Go object struct together with its behavior and is
//! verified against the Go source under `conduit/internal/objects`. Values are
//! kept as typed structs (not bare `serde_json::Value`) per `RUST-P3-005`
//! OBJ-* items.

pub mod apikey;
pub mod channel_settings;
pub mod condition;
pub mod cost;
pub mod model;
pub mod model_association;
pub mod money;
pub mod overrides;
pub mod pricing;
pub mod project;
pub mod prompt;
pub mod prompt_protection;
pub mod response;
pub mod storage;
pub mod user;

pub use condition::{Condition, ConditionError, ConditionType, evaluate, validate};
pub use model::{
    DeveloperModelSettings, ModelCard, ModelCardCost, ModelCardLimit, ModelCardModalities,
    ModelCardReasoning, ModelSettings, SystemModelSettings,
};
pub use model_association::{
    ChannelModelAssociation, ChannelRegexAssociation, ChannelTagsModelAssociation,
    ChannelTagsRegexAssociation, ExcludeAssociation, ModelAssociation, ModelAssociationWhen,
    ModelIDAssociation, RegexAssociation,
};
