#!/usr/bin/env node

/**
 * Repository-specific Rust security invariants.
 *
 * This deliberately complements (rather than imitates) Clippy. The checks are
 * limited to patterns that previously caused concrete Conduit API findings,
 * so the release gate remains actionable and low-noise.
 */

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(scriptDirectory, "..", "..");

const boundaryCrates = [
  "crates/conduit-http/src",
  "crates/conduit-admin-graphql/src",
  "crates/conduit-openapi-graphql/src",
];

const sensitiveLogIdentifier =
  /\b(?:authorization|api_key|access_token|refresh_token|id_token|jwt_secret|client_secret|password|provider_body|prompt|request_body|response_body|dsn)\b/i;
const explicitlySafeLogIdentifier =
  /\b(?:has_|is_|redacted|masked|hash|length|count|status|kind|class|id|error)\w*\b/i;

// HLT-002 debt: legacy wiring traits do not consistently receive the request
// context, so these adapters construct test principals as a fallback. Counts
// are exact: adding an occurrence fails, and removing one makes the baseline
// stale until this list is reduced in the same change.
const testPrincipalBaseline = new Map([
  ["crates/conduit-bin/src/wiring_apikey.rs", 1],
  ["crates/conduit-bin/src/wiring_channel_crud.rs", 1],
  ["crates/conduit-bin/src/wiring_channel_ext.rs", 1],
  ["crates/conduit-bin/src/wiring_data_storage.rs", 1],
  ["crates/conduit-bin/src/wiring_postgres_project_role.rs", 1],
  ["crates/conduit-bin/src/wiring_product_experience.rs", 1],
  ["crates/conduit-bin/src/wiring_prompt.rs", 1],
  ["crates/conduit-bin/src/wiring_request_content.rs", 1],
  ["crates/conduit-bin/src/wiring_request_execution.rs", 1],
  ["crates/conduit-bin/src/wiring_requests.rs", 2],
  ["crates/conduit-bin/src/wiring_system_settings_ext.rs", 1],
  ["crates/conduit-orchestrator/src/db_candidate_source.rs", 1],
]);

function normalize(file) {
  return file.split(path.sep).join("/");
}

function rustFiles(directory) {
  const files = [];
  for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
    const fullPath = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...rustFiles(fullPath));
    } else if (entry.isFile() && entry.name.endsWith(".rs")) {
      files.push(fullPath);
    }
  }
  return files;
}

// Inline unit-test modules in this repository are conventionally at the end
// of a source file. Integration tests live under tests/ and are never scanned.
function productionPrefix(source) {
  const marker = source.search(/^\s*#\[cfg\(test\)\]\s*$/m);
  return marker === -1 ? source : source.slice(0, marker);
}

// Preserve line breaks and code identifiers while blanking comments and
// literals. This is enough for the narrow token checks below and keeps line
// numbers stable without introducing a Rust parser dependency.
function maskCommentsAndLiterals(source) {
  let output = "";
  let index = 0;
  let blockDepth = 0;
  let state = "code";

  while (index < source.length) {
    const current = source[index];
    const next = source[index + 1];

    if (state === "line-comment") {
      if (current === "\n") {
        state = "code";
        output += "\n";
      } else {
        output += " ";
      }
      index += 1;
      continue;
    }

    if (state === "block-comment") {
      if (current === "/" && next === "*") {
        blockDepth += 1;
        output += "  ";
        index += 2;
      } else if (current === "*" && next === "/") {
        blockDepth -= 1;
        output += "  ";
        index += 2;
        if (blockDepth === 0) state = "code";
      } else {
        output += current === "\n" ? "\n" : " ";
        index += 1;
      }
      continue;
    }

    if (state === "string" || state === "char") {
      const terminator = state === "string" ? '"' : "'";
      if (current === "\\") {
        output += "  ";
        index += Math.min(2, source.length - index);
      } else {
        output += current === "\n" ? "\n" : " ";
        index += 1;
        if (current === terminator) state = "code";
      }
      continue;
    }

    if (current === "/" && next === "/") {
      state = "line-comment";
      output += "  ";
      index += 2;
    } else if (current === "/" && next === "*") {
      state = "block-comment";
      blockDepth = 1;
      output += "  ";
      index += 2;
    } else if (current === '"') {
      state = "string";
      output += " ";
      index += 1;
    } else if (current === "'") {
      // Lifetimes are identifiers, not character literals.
      const lifetime = source.slice(index).match(/^'[A-Za-z_][A-Za-z0-9_]*/);
      if (lifetime && source[index + lifetime[0].length] !== "'") {
        output += lifetime[0];
        index += lifetime[0].length;
      } else {
        state = "char";
        output += " ";
        index += 1;
      }
    } else {
      output += current;
      index += 1;
    }
  }
  return output;
}

function lineNumber(source, index) {
  return source.slice(0, index).split("\n").length;
}

function findMatches(source, pattern) {
  return [...source.matchAll(new RegExp(pattern.source, `${pattern.flags.replace("g", "")}g`))];
}

function extractLoggingMacros(maskedSource) {
  const starts = findMatches(
    maskedSource,
    /(?:tracing::)?(?:trace|debug|info|warn|error)!\s*\(/g,
  );
  const invocations = [];
  for (const start of starts) {
    const open = maskedSource.indexOf("(", start.index);
    let depth = 0;
    for (let index = open; index < maskedSource.length; index += 1) {
      if (maskedSource[index] === "(") depth += 1;
      if (maskedSource[index] === ")") depth -= 1;
      if (depth === 0) {
        invocations.push({ index: start.index, body: maskedSource.slice(open + 1, index) });
        break;
      }
    }
  }
  return invocations;
}

function scanSource(relativePath, source) {
  const findings = [];
  const production = productionPrefix(source);
  const masked = maskCommentsAndLiterals(production);

  if (!relativePath.includes("/tests/")) {
    for (const match of findMatches(masked, /\bPrincipal::test\s*\(/g)) {
      findings.push({
        rule: "RUST-AUTH-001",
        line: lineNumber(masked, match.index),
        message: "test bypass principal is reachable from production code",
      });
    }
  }

  if (boundaryCrates.some((directory) => relativePath.startsWith(`${directory}/`))) {
    for (const match of findMatches(masked, /\bPrincipal::system\s*\(/g)) {
      findings.push({
        rule: "RUST-AUTH-002",
        line: lineNumber(masked, match.index),
        message: "system bypass principal must not be constructed at an HTTP/GraphQL boundary",
      });
    }
  }

  if (
    relativePath.startsWith("crates/conduit-db/src/") ||
    boundaryCrates.some((directory) => relativePath.startsWith(`${directory}/`))
  ) {
    for (const match of findMatches(
      masked,
      /\bproject_id\s*:\s*[^\n,;}]*(?:unwrap_or_default\s*\(\)|unwrap_or\s*\(\s*(?:String::new\s*\(\)|Default::default\s*\(\))\s*\))/g,
    )) {
      findings.push({
        rule: "RUST-AUTH-003",
        line: lineNumber(masked, match.index),
        message: "authorization/query project_id silently defaults to an empty value",
      });
    }
  }

  for (const invocation of extractLoggingMacros(masked)) {
    const identifiers = invocation.body.match(/\b[A-Za-z_][A-Za-z0-9_]*\b/g) ?? [];
    const unsafe = identifiers.find(
      (identifier) =>
        sensitiveLogIdentifier.test(identifier) && !explicitlySafeLogIdentifier.test(identifier),
    );
    if (unsafe) {
      findings.push({
        rule: "RUST-LOG-001",
        line: lineNumber(masked, invocation.index),
        message: `sensitive value '${unsafe}' is passed to a logging macro; log only redacted metadata`,
      });
    }
  }

  for (const match of findMatches(masked, /\.danger_accept_invalid_certs\s*\(\s*true\s*\)/g)) {
    findings.push({
      rule: "RUST-TLS-006",
      line: lineNumber(masked, match.index),
      message: "TLS verification is disabled unconditionally instead of through reviewed configuration",
    });
  }

  return findings;
}

function requireSnippet(relativePath, snippet, rule, findings) {
  const source = fs.readFileSync(path.join(repositoryRoot, relativePath), "utf8");
  if (!source.includes(snippet)) {
    findings.push({ rule, file: relativePath, line: 1, message: `required invariant missing: ${snippet}` });
  }
}

function scanRepository() {
  const findings = [];
  for (const fullPath of rustFiles(path.join(repositoryRoot, "crates"))) {
    const relativePath = normalize(path.relative(repositoryRoot, fullPath));
    if (relativePath.includes("/tests/")) continue;
    const source = fs.readFileSync(fullPath, "utf8");
    for (const finding of scanSource(relativePath, source)) {
      findings.push({ ...finding, file: relativePath });
    }
  }

  const testPrincipalFindings = findings.filter(({ rule }) => rule === "RUST-AUTH-001");
  for (const [file, expected] of testPrincipalBaseline) {
    const actual = testPrincipalFindings.filter((finding) => finding.file === file).length;
    if (actual !== expected) {
      findings.push({
        rule: "RUST-AUTH-BASELINE",
        file,
        line: 1,
        message: `test-principal baseline is stale (found ${actual}, expected ${expected}); reduce the baseline when debt is removed`,
      });
    }
  }
  for (let index = findings.length - 1; index >= 0; index -= 1) {
    const finding = findings[index];
    if (
      finding.rule === "RUST-AUTH-001" &&
      testPrincipalBaseline.has(finding.file) &&
      testPrincipalFindings.filter((candidate) => candidate.file === finding.file).length ===
        testPrincipalBaseline.get(finding.file)
    ) {
      findings.splice(index, 1);
    }
  }

  // Positive checks prevent a future refactor from silently dropping one of
  // the TLS or project-isolation links while still avoiding broad heuristics.
  requireSnippet(
    "crates/conduit-bin/src/wiring.rs",
    "config.server.disable_ssl_verify,",
    "RUST-TLS-001",
    findings,
  );
  requireSnippet(
    "crates/conduit-bin/src/wiring.rs",
    ".insecure_skip_verify(insecure_skip_verify)",
    "RUST-TLS-002",
    findings,
  );
  requireSnippet(
    "crates/conduit-bin/src/wiring.rs",
    ".with_insecure_skip_verify(insecure_skip_verify)",
    "RUST-TLS-003",
    findings,
  );
  requireSnippet(
    "crates/conduit-orchestrator/src/upstream_executor.rs",
    ".insecure_skip_verify(self.insecure_skip_verify)",
    "RUST-TLS-004",
    findings,
  );
  requireSnippet(
    "crates/conduit-config/src/loader.rs",
    '"CONDUIT_SERVER_DISABLE_SSL_VERIFY"',
    "RUST-TLS-005",
    findings,
  );
  for (const method of ["aggregate_usage", "count_usage", "list_usage"]) {
    requireSnippet(
      "crates/conduit-db/src/repo/usage_repo.rs",
      `async fn ${method}`,
      "RUST-AUTH-004",
      findings,
    );
  }
  const usageRepo = fs.readFileSync(
    path.join(repositoryRoot, "crates/conduit-db/src/repo/usage_repo.rs"),
    "utf8",
  );
  const usageGuards = findMatches(
    maskCommentsAndLiterals(productionPrefix(usageRepo)),
    /guard_project_access\s*\(\s*ctx\s*,\s*&query\.project_id\s*,\s*ProjectAccess::Read\s*\)\s*\?/g,
  ).length;
  if (usageGuards < 3) {
    findings.push({
      rule: "RUST-AUTH-004",
      file: "crates/conduit-db/src/repo/usage_repo.rs",
      line: 1,
      message: `usage list/count/aggregate must remain project-guarded (found ${usageGuards}, expected at least 3)`,
    });
  }
  return findings;
}

function selfTest() {
  assert.equal(scanSource("crates/conduit-http/src/handler.rs", "fn f() { Principal::system(); }").length, 1);
  assert.equal(scanSource("crates/conduit-bin/src/job.rs", "fn f() { Principal::test(); }").length, 1);
  assert.equal(
    scanSource(
      "crates/conduit-http/src/handler.rs",
      "// Principal::system()\nfn f() {}\n#[cfg(test)]\nmod tests { fn t() { Principal::test(); } }",
    ).length,
    0,
  );
  assert.equal(
    scanSource("crates/conduit-db/src/repo/x.rs", "let row = Q { project_id: value.unwrap_or_default() };")[0].rule,
    "RUST-AUTH-003",
  );
  assert.equal(
    scanSource("crates/conduit-bin/src/job.rs", 'tracing::info!(api_key = %api_key, "connected");')[0].rule,
    "RUST-LOG-001",
  );
  assert.equal(
    scanSource("crates/conduit-bin/src/job.rs", 'tracing::info!(api_key_id = %api_key_id, "connected");').length,
    0,
  );
  assert.equal(
    scanSource("crates/conduit-bin/src/job.rs", "let c = 'x'; tracing::warn!(password = %password);")[0].rule,
    "RUST-LOG-001",
  );
  assert.equal(
    scanSource("crates/conduit-llm/src/client.rs", "builder.danger_accept_invalid_certs(true);")[0].rule,
    "RUST-TLS-006",
  );
  process.stdout.write("Rust security static check self-test passed (8 cases).\n");
}

if (process.argv.includes("--self-test")) {
  selfTest();
} else {
  const findings = scanRepository();
  if (findings.length > 0) {
    for (const finding of findings) {
      process.stderr.write(
        `::error file=${finding.file},line=${finding.line},title=${finding.rule}::${finding.message}\n`,
      );
    }
    process.stderr.write(`Rust security static check failed with ${findings.length} finding(s).\n`);
    process.exitCode = 1;
  } else {
    const baselineCount = [...testPrincipalBaseline.values()].reduce((sum, count) => sum + count, 0);
    process.stdout.write(
      `Rust security static check passed (${baselineCount} legacy test-principal occurrence(s) remain baselined).\n`,
    );
  }
}
