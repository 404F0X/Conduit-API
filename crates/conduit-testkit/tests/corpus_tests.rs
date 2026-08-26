//! Corpus pin test for `tests/contracts/llm_cases/` (workspace root).
//!
//! Every golden case JSON transcribed from the Go transformer tests must keep
//! satisfying the stable container shape enforced by `GoldenCase::parse`
//! (six required sections, optional ordered `events` array of objects,
//! `$ANY_*` placeholder whitelist — see the corpus README.md). The corpus is
//! the contract: the parser converges to the corpus, never the reverse.
//!
//! Two regressions are pinned here:
//! 1. shape drift — any case file `GoldenCase::parse` rejects fails the test,
//!    reporting *all* offending files (no fail-fast);
//! 2. accidental corpus deletion — the total case count must stay `>= 33`
//!    (`>=`, not `==`, so adding cases never breaks the pin).

use std::fs;
use std::path::{Path, PathBuf};

use conduit_testkit::GoldenCase;

/// Known corpus floor: 33 case files across the provider directories listed
/// in the README.md case index (openai_chat, anthropic, gemini, doubao, jina,
/// aisdk, openai_responses, openai_image, openai_audio).
const MIN_CASES: usize = 33;

/// Corpus directory relative to the workspace root, built component-wise from
/// `CARGO_MANIFEST_DIR` (`<workspace>/crates/conduit-testkit`) so the path is
/// separator-correct on Windows: `../../tests/contracts/llm_cases`.
fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("contracts")
        .join("llm_cases")
}

/// Recursively collect every `*.json` file under `dir` into `out`.
/// Non-JSON files (e.g. `README.md`) are skipped by the extension filter.
/// Entries are sorted per directory so failure output is deterministic.
fn collect_json_files(
    dir: &Path,
    out: &mut Vec<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut children = Vec::new();
    for entry in fs::read_dir(dir)? {
        children.push(entry?.path());
    }
    children.sort();

    for path in children {
        if path.is_dir() {
            // Provider subdirectory (one level today, but recurse for safety).
            collect_json_files(&path, out)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            out.push(path);
        }
    }
    Ok(())
}

#[test]
fn every_llm_case_json_parses_as_golden_case() -> Result<(), Box<dyn std::error::Error>> {
    let root = corpus_dir();
    let mut files = Vec::new();
    collect_json_files(&root, &mut files)
        .map_err(|error| format!("cannot enumerate corpus dir {}: {error}", root.display()))?;

    // Run GoldenCase::parse over every file, collecting ALL failures
    // (file + error) before asserting, so one run surfaces every regression.
    let mut failures = Vec::new();
    for path in &files {
        // Report paths relative to the corpus root: short and machine-neutral.
        let name = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string();
        let outcome = fs::read_to_string(path)
            .map_err(|error| format!("read error: {error}"))
            .and_then(|input| GoldenCase::parse(&input).map(|_case| ()));
        if let Err(error) = outcome {
            failures.push(format!("  {name}: {error}"));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} golden case files failed GoldenCase::parse:\n{}",
        failures.len(),
        files.len(),
        failures.join("\n")
    );

    // Corpus-size pin: guards against accidental deletion of case files.
    assert!(
        files.len() >= MIN_CASES,
        "corpus shrank: found {} golden case files under {}, expected at least {MIN_CASES}",
        files.len(),
        root.display()
    );
    Ok(())
}
