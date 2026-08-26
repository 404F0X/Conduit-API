use std::{
    env,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");
    println!("cargo:rerun-if-env-changed=TARGET");
    // Allow Docker build-args (or any external env) to override build metadata.
    // Precedence: explicit CONDUIT_VERSION / CONDUIT_COMMIT / CONDUIT_BUILD_TIME
    // env var > git / cargo computed value > "unknown".
    for key in [
        "CONDUIT_VERSION",
        "CONDUIT_COMMIT",
        "CONDUIT_BUILD_TIME",
        "CONDUIT_BRANCH",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
    }

    set_build_env(
        "CONDUIT_BUILD_VERSION",
        override_or(
            "CONDUIT_VERSION",
            env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".to_string()),
        ),
    );
    set_build_env(
        "CONDUIT_BUILD_COMMIT",
        override_or(
            "CONDUIT_COMMIT",
            git_output(&["rev-parse", "--short=12", "HEAD"]),
        ),
    );
    set_build_env(
        "CONDUIT_BUILD_BRANCH",
        override_or(
            "CONDUIT_BRANCH",
            git_output(&["rev-parse", "--abbrev-ref", "HEAD"]),
        ),
    );
    set_build_env(
        "CONDUIT_BUILD_TIME",
        override_or("CONDUIT_BUILD_TIME", build_time()),
    );
    set_build_env("CONDUIT_BUILD_RUSTC_VERSION", rustc_version());
    set_build_env(
        "CONDUIT_BUILD_TARGET",
        env::var("TARGET").unwrap_or_else(|_| "unknown".to_string()),
    );
}

fn set_build_env(key: &str, value: String) {
    println!("cargo:rustc-env={key}={}", sanitize(value));
}

fn git_output(args: &[&str]) -> String {
    command_output("git", args).unwrap_or_else(|| "unknown".to_string())
}

fn rustc_version() -> String {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    command_output(&rustc, &["--version"]).unwrap_or_else(|| "unknown".to_string())
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    let trimmed = stdout.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn build_time() -> String {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => format!("unix_seconds:{}", duration.as_secs()),
        Err(_) => "unknown".to_string(),
    }
}

fn sanitize(value: String) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed.replace(['\r', '\n'], " ")
    }
}

/// Return the explicit override env var when it is set to a non-empty value,
/// otherwise fall back to the computed default. Used to let Docker `--build-arg`
/// values (surfaced as env vars in the build stage) override build metadata.
fn override_or(env_key: &str, default: String) -> String {
    match env::var(env_key) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => default,
    }
}
