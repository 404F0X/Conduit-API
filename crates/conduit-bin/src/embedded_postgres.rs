use std::{env, io::IsTerminal};

use conduit_config::AppConfig;

const DATABASE_MODE_ENV: &str = "CONDUIT_DATABASE_MODE";
const DATABASE_DSN_ENV: &str = "CONDUIT_DB_DSN";
const EMBEDDED_DIRECTORY_ENV: &str = "CONDUIT_EMBEDDED_POSTGRES_DIR";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DatabaseMode {
    Embedded,
    External,
    Prompt,
}

pub(crate) struct ManagedDatabase {
    #[cfg(all(windows, feature = "embedded-postgres"))]
    _inner: windows::WindowsManagedDatabase,
}

pub(crate) fn prepare(
    config: &mut AppConfig,
    config_source_exists: bool,
) -> Result<Option<ManagedDatabase>, String> {
    let mode = resolve_mode(
        env::var(DATABASE_MODE_ENV).ok().as_deref(),
        env::var_os(DATABASE_DSN_ENV).is_some(),
        config_source_exists,
        embedded_state_exists(),
        std::io::stdin().is_terminal() && cfg!(all(windows, feature = "embedded-postgres")),
    )?;
    let mode = if mode == DatabaseMode::Prompt {
        prompt_for_mode()?
    } else {
        mode
    };

    match mode {
        DatabaseMode::External => Ok(None),
        DatabaseMode::Embedded => start_embedded(config).map(Some),
        DatabaseMode::Prompt => Err("database setup choice was not resolved".to_string()),
    }
}

fn resolve_mode(
    configured_mode: Option<&str>,
    dsn_environment_set: bool,
    config_source_exists: bool,
    embedded_state_exists: bool,
    interactive: bool,
) -> Result<DatabaseMode, String> {
    if let Some(mode) = configured_mode
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return match mode.to_ascii_lowercase().as_str() {
            "embedded" => {
                if dsn_environment_set {
                    Err(format!(
                        "{DATABASE_MODE_ENV}=embedded conflicts with {DATABASE_DSN_ENV}; remove one of them"
                    ))
                } else {
                    Ok(DatabaseMode::Embedded)
                }
            }
            "external" => Ok(DatabaseMode::External),
            _ => Err(format!(
                "{DATABASE_MODE_ENV} must be `embedded` or `external`"
            )),
        };
    }
    if dsn_environment_set || config_source_exists {
        return Ok(DatabaseMode::External);
    }
    if embedded_state_exists {
        return Ok(DatabaseMode::Embedded);
    }
    if interactive {
        Ok(DatabaseMode::Prompt)
    } else {
        Ok(DatabaseMode::External)
    }
}

fn prompt_for_mode() -> Result<DatabaseMode, String> {
    use std::io::{self, Write};

    let mut output = io::stdout().lock();
    writeln!(output, "Conduit API first-time database setup").map_err(|error| error.to_string())?;
    writeln!(
        output,
        "  1. Managed local PostgreSQL (recommended; no separate database setup)"
    )
    .map_err(|error| error.to_string())?;
    writeln!(
        output,
        "  2. External PostgreSQL (configure config.yml or {DATABASE_DSN_ENV})"
    )
    .map_err(|error| error.to_string())?;
    write!(output, "Select database mode [1]: ").map_err(|error| error.to_string())?;
    output.flush().map_err(|error| error.to_string())?;
    drop(output);

    let mut choice = String::new();
    io::stdin()
        .read_line(&mut choice)
        .map_err(|error| format!("failed to read database setup choice: {error}"))?;
    match choice.trim() {
        "" | "1" => Ok(DatabaseMode::Embedded),
        "2" => Err(format!(
            "external PostgreSQL selected; create config.yml or set {DATABASE_DSN_ENV}, then start Conduit API again"
        )),
        _ => Err("database setup choice must be 1 or 2".to_string()),
    }
}

#[cfg(all(windows, feature = "embedded-postgres"))]
fn start_embedded(config: &mut AppConfig) -> Result<ManagedDatabase, String> {
    windows::start(config).map(|inner| ManagedDatabase { _inner: inner })
}

#[cfg(not(all(windows, feature = "embedded-postgres")))]
fn start_embedded(_config: &mut AppConfig) -> Result<ManagedDatabase, String> {
    Err(
        "managed local PostgreSQL is available in the official Windows release; use external PostgreSQL on this build"
            .to_string(),
    )
}

#[cfg(windows)]
fn embedded_state_exists() -> bool {
    windows_state_directory()
        .map(|directory| directory.join("state.json").is_file())
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn embedded_state_exists() -> bool {
    false
}

#[cfg(windows)]
fn windows_state_directory() -> Result<std::path::PathBuf, String> {
    let directory = if let Some(value) = env::var_os(EMBEDDED_DIRECTORY_ENV) {
        let value = std::path::PathBuf::from(value);
        if !value.is_absolute() {
            return Err(format!("{EMBEDDED_DIRECTORY_ENV} must be an absolute path"));
        }
        value
    } else {
        let local_app_data = env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| {
                "LOCALAPPDATA is unavailable; cannot locate managed database data".to_string()
            })?;
        local_app_data
            .join("Conduit API")
            .join("embedded-postgresql")
    };
    Ok(directory)
}

#[cfg(all(windows, feature = "embedded-postgres"))]
mod windows {
    use std::{
        collections::HashMap,
        fs::{self, File, OpenOptions},
        io::Write,
        net::TcpListener,
        path::Path,
        time::Duration,
    };

    use conduit_config::AppConfig;
    use fs2::FileExt;
    use postgresql_embedded::{Settings, Status, VersionReq, blocking::PostgreSQL};
    use serde::{Deserialize, Serialize};
    use url::Url;
    use uuid::Uuid;

    use super::windows_state_directory;

    pub(super) const BUNDLED_POSTGRESQL_VERSION: &str = "17.11.0";
    const POSTGRESQL_RELEASES_URL: &str = "https://github.com/theseus-rs/postgresql-binaries";
    const STATE_SCHEMA_VERSION: u8 = 1;
    const APP_DATABASE: &str = "conduit";
    const APP_ROLE: &str = "conduit";

    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct EmbeddedState {
        schema_version: u8,
        postgresql_version: String,
        port: u16,
        admin_password: String,
        app_password: String,
    }

    pub(crate) struct WindowsManagedDatabase {
        _postgresql: PostgreSQL,
        _lock: File,
    }

    pub(super) fn start(config: &mut AppConfig) -> Result<WindowsManagedDatabase, String> {
        let base = windows_state_directory()?;
        fs::create_dir_all(&base)
            .map_err(|error| format!("failed to create managed database directory: {error}"))?;
        let lock = acquire_lock(&base)?;
        let state = load_or_create_state(&base)?;

        let version = VersionReq::parse(&format!("={BUNDLED_POSTGRESQL_VERSION}"))
            .map_err(|error| format!("invalid bundled PostgreSQL version: {error}"))?;
        let settings = Settings {
            releases_url: POSTGRESQL_RELEASES_URL.to_string(),
            version,
            installation_dir: base.join("runtime"),
            password_file: base.join("admin-password"),
            data_dir: base.join("data"),
            host: "127.0.0.1".to_string(),
            port: state.port,
            username: "postgres".to_string(),
            password: state.admin_password.clone(),
            temporary: false,
            timeout: Some(Duration::from_secs(90)),
            configuration: HashMap::from([
                ("listen_addresses".to_string(), "127.0.0.1".to_string()),
                ("max_connections".to_string(), "50".to_string()),
            ]),
            trust_installation_dir: false,
        };
        let first_initialization = !settings.data_dir.join("postgresql.conf").is_file();
        let mut postgresql = PostgreSQL::new(settings);
        if first_initialization {
            println!("Installing and initializing managed local PostgreSQL...");
        } else {
            println!("Starting managed local PostgreSQL...");
        }
        postgresql
            .setup()
            .map_err(|error| format!("managed PostgreSQL setup failed: {error}"))?;
        if postgresql.status() != Status::Started {
            postgresql
                .start()
                .map_err(|error| format!("managed PostgreSQL start failed: {error}"))?;
        }

        let admin_dsn = postgresql.settings().url("postgres");
        let app_dsn = app_dsn(&admin_dsn, &state.app_password)?;
        crate::cli::bootstrap_postgres_from_dsns(&admin_dsn, &app_dsn)?;
        config.db.dialect = "postgres".to_string();
        config.db.dsn = app_dsn;
        println!(
            "Managed PostgreSQL is ready. Data is stored in {}",
            base.display()
        );
        Ok(WindowsManagedDatabase {
            _postgresql: postgresql,
            _lock: lock,
        })
    }

    fn acquire_lock(base: &Path) -> Result<File, String> {
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(base.join("conduit.lock"))
            .map_err(|error| format!("failed to open managed database lock: {error}"))?;
        lock.try_lock_exclusive().map_err(|_| {
            "managed PostgreSQL is already in use by another Conduit API process".to_string()
        })?;
        Ok(lock)
    }

    fn load_or_create_state(base: &Path) -> Result<EmbeddedState, String> {
        let state_path = base.join("state.json");
        if state_path.is_file() {
            return load_state(&state_path);
        }
        if base.join("data").join("postgresql.conf").is_file() {
            return Err(
                "managed PostgreSQL data exists but state.json is missing; refusing to replace credentials"
                    .to_string(),
            );
        }
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|error| format!("failed to reserve a managed PostgreSQL port: {error}"))?;
        let port = listener
            .local_addr()
            .map_err(|error| format!("failed to read managed PostgreSQL port: {error}"))?
            .port();
        drop(listener);
        let state = EmbeddedState {
            schema_version: STATE_SCHEMA_VERSION,
            postgresql_version: BUNDLED_POSTGRESQL_VERSION.to_string(),
            port,
            admin_password: random_password(),
            app_password: random_password(),
        };
        write_state_atomically(&state_path, &state)?;
        Ok(state)
    }

    fn load_state(path: &Path) -> Result<EmbeddedState, String> {
        let encoded = fs::read_to_string(path)
            .map_err(|error| format!("failed to read managed PostgreSQL state: {error}"))?;
        let state: EmbeddedState = serde_json::from_str(&encoded)
            .map_err(|error| format!("managed PostgreSQL state is invalid: {error}"))?;
        if state.schema_version != STATE_SCHEMA_VERSION
            || state.postgresql_version != BUNDLED_POSTGRESQL_VERSION
            || state.port == 0
            || !valid_password(&state.admin_password)
            || !valid_password(&state.app_password)
        {
            return Err("managed PostgreSQL state contains unsupported values".to_string());
        }
        Ok(state)
    }

    fn write_state_atomically(path: &Path, state: &EmbeddedState) -> Result<(), String> {
        let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4().simple()));
        let encoded = serde_json::to_vec(state)
            .map_err(|error| format!("failed to encode managed PostgreSQL state: {error}"))?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| format!("failed to create managed PostgreSQL state: {error}"))?;
        file.write_all(&encoded)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("failed to persist managed PostgreSQL state: {error}"))?;
        drop(file);
        fs::rename(&temporary, path)
            .map_err(|error| format!("failed to publish managed PostgreSQL state: {error}"))
    }

    fn random_password() -> String {
        format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
    }

    fn valid_password(value: &str) -> bool {
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    }

    fn app_dsn(admin_dsn: &str, password: &str) -> Result<String, String> {
        let mut url = Url::parse(admin_dsn)
            .map_err(|error| format!("managed PostgreSQL returned an invalid URL: {error}"))?;
        url.set_username(APP_ROLE)
            .map_err(|_| "failed to configure managed PostgreSQL role".to_string())?;
        url.set_password(Some(password))
            .map_err(|_| "failed to configure managed PostgreSQL password".to_string())?;
        url.set_path(APP_DATABASE);
        Ok(url.to_string())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn app_connection_uses_unprivileged_role_and_database() -> Result<(), String> {
            let dsn = app_dsn(
                "postgresql://postgres:admin@127.0.0.1:55432/postgres",
                "app-password",
            )?;
            assert_eq!(
                dsn,
                "postgresql://conduit:app-password@127.0.0.1:55432/conduit"
            );
            Ok(())
        }

        #[test]
        fn generated_passwords_are_high_entropy_ascii() {
            let first = random_password();
            let second = random_password();
            assert!(valid_password(&first));
            assert!(valid_password(&second));
            assert_ne!(first, second);
        }

        #[test]
        fn bootstrap_errors_redact_urls_and_decoded_passwords() {
            let admin = "postgresql://postgres:admin-secret@127.0.0.1:5432/postgres";
            let target = "postgresql://conduit:app%40secret@127.0.0.1:5432/conduit";
            let redacted = crate::cli::redact_bootstrap_error(
                format!("failed with {admin}, {target}, admin-secret, and app@secret"),
                admin,
                target,
            );
            assert!(!redacted.contains(admin));
            assert!(!redacted.contains(target));
            assert!(!redacted.contains("admin-secret"));
            assert!(!redacted.contains("app@secret"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_database_configuration_wins_over_existing_managed_state() {
        assert_eq!(
            resolve_mode(None, true, false, true, true),
            Ok(DatabaseMode::External)
        );
        assert_eq!(
            resolve_mode(None, false, true, true, true),
            Ok(DatabaseMode::External)
        );
    }

    #[test]
    fn managed_state_is_reused_and_new_interactive_installs_prompt() {
        assert_eq!(
            resolve_mode(None, false, false, true, false),
            Ok(DatabaseMode::Embedded)
        );
        assert_eq!(
            resolve_mode(None, false, false, false, true),
            Ok(DatabaseMode::Prompt)
        );
        assert_eq!(
            resolve_mode(None, false, false, false, false),
            Ok(DatabaseMode::External)
        );
    }

    #[test]
    fn explicit_embedded_mode_rejects_dsn_conflict_and_unknown_values() {
        assert!(resolve_mode(Some("embedded"), true, false, false, false).is_err());
        assert!(resolve_mode(Some("sqlite"), false, false, false, false).is_err());
        assert_eq!(
            resolve_mode(Some("external"), false, false, true, true),
            Ok(DatabaseMode::External)
        );
    }
}
