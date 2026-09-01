#![forbid(unsafe_code)]

mod auto_disable_runtime;
mod cli;
mod conv;
mod maintenance;
mod model_fetch;
mod model_matcher;
#[cfg(test)]
mod postgres_test_support;
mod route_affinity;
mod runtime_logging;
mod usage_charge_settler;
mod usage_charge_settler_postgres;
mod usage_log_recorder;
mod wiring;
mod wiring_apikey;
mod wiring_channel_crud;
mod wiring_channel_ext;
mod wiring_channel_override_template;
mod wiring_data_storage;
mod wiring_model_catalog;
mod wiring_oauth_admin;
mod wiring_oidc;
mod wiring_openapi;
mod wiring_operations;
mod wiring_postgres_auth;
mod wiring_postgres_backup;
mod wiring_postgres_billing;
mod wiring_postgres_change_sets;
mod wiring_postgres_channel_model_sync;
mod wiring_postgres_channel_probe;
mod wiring_postgres_channel_probe_query;
mod wiring_postgres_commercialization;
mod wiring_postgres_dashboard;
mod wiring_postgres_identity;
mod wiring_postgres_model_market;
mod wiring_postgres_observability;
mod wiring_postgres_openapi;
mod wiring_postgres_operations;
mod wiring_postgres_pricing_admission;
mod wiring_postgres_project_role;
mod wiring_postgres_provider_pricing;
mod wiring_postgres_provider_quota;
mod wiring_postgres_quota;
mod wiring_postgres_redemption;
mod wiring_postgres_simple_group;
mod wiring_postgres_system_initialize;
mod wiring_postgres_system_operations;
mod wiring_postgres_user;
mod wiring_postgres_video_storage;
mod wiring_product_experience;
mod wiring_profile_template;
mod wiring_project_access;
mod wiring_prompt;
mod wiring_quota_common;
mod wiring_request_content;
mod wiring_request_execution;
mod wiring_requests;
mod wiring_route_explanation;
mod wiring_route_health;
mod wiring_system_settings_ext;
mod wiring_video;

use std::process::ExitCode;

fn main() -> ExitCode {
    match cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::from(err.exit_code())
        }
    }
}
