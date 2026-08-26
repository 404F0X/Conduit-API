use std::path::Path;

use conduit_config::model::LogConfig;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::prelude::*;

pub fn init(
    config: &LogConfig,
) -> Result<Option<tracing_appender::non_blocking::WorkerGuard>, String> {
    let filter = build_filter(config)?;
    let json = matches!(config.encoding.as_str(), "json" | "console_json")
        || matches!(config.format.as_str(), "json" | "console_json");

    if config.output.eq_ignore_ascii_case("file") {
        let path = Path::new(&config.file.path);
        let directory = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(directory)
            .map_err(|error| format!("failed to create log directory {directory:?}: {error}"))?;
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("conduit.log");
        let max_files = usize::try_from(config.file.max_backups.max(1)).unwrap_or(10);
        let appender = tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix(filename)
            .max_log_files(max_files)
            .build(directory)
            .map_err(|error| format!("failed to initialize file logger: {error}"))?;
        let (writer, guard) = tracing_appender::non_blocking(appender);
        if json {
            tracing_subscriber::registry()
                .with(filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .json()
                        .with_writer(writer)
                        .with_span_events(FmtSpan::CLOSE),
                )
                .try_init()
                .map_err(|error| format!("failed to install tracing subscriber: {error}"))?;
        } else {
            tracing_subscriber::registry()
                .with(filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(writer)
                        .with_span_events(FmtSpan::CLOSE),
                )
                .try_init()
                .map_err(|error| format!("failed to install tracing subscriber: {error}"))?;
        }
        return Ok(Some(guard));
    }

    let writer = if config.stdout {
        tracing_subscriber::fmt::writer::BoxMakeWriter::new(std::io::stdout)
    } else {
        tracing_subscriber::fmt::writer::BoxMakeWriter::new(std::io::stderr)
    };
    if json {
        tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(writer)
                    .with_span_events(FmtSpan::CLOSE),
            )
            .try_init()
            .map_err(|error| format!("failed to install tracing subscriber: {error}"))?;
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(writer)
                    .with_span_events(FmtSpan::CLOSE),
            )
            .try_init()
            .map_err(|error| format!("failed to install tracing subscriber: {error}"))?;
    }
    Ok(None)
}

fn build_filter(config: &LogConfig) -> Result<EnvFilter, String> {
    let level = if config.debug {
        "debug"
    } else {
        config.level.as_str()
    };
    let mut directives = vec![level.to_string()];
    directives.extend(
        config
            .includes
            .iter()
            .map(|target| format!("{target}={level}")),
    );
    directives.extend(config.excludes.iter().map(|target| format!("{target}=off")));
    EnvFilter::try_new(directives.join(","))
        .map_err(|error| format!("invalid log filter configuration: {error}"))
}
