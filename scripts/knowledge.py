"""Project graph CLI, with explicit refresh and content-based freshness checks.

Local executable/runtime/cache paths live in ignored .codebase-memory/client.json.
The graph engine remains codebase-memory-mcp; this script is not a second indexer.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parents[1]
CACHE = ROOT / ".codebase-memory"
READ_TOOLS = {
    "search_graph", "trace_path", "get_code_snippet", "query_graph",
    "get_architecture", "get_file_outline", "get_graph_schema",
    "check_index_coverage", "index_status", "detect_changes", "search_code",
}
# Include contracts, deployment and CI inputs as well as application source.
# Markdown/task state do not contain parsed product symbols and can evolve
# without invalidating the product graph's content identity.
INPUT_EXCLUDES = [".codex-workflow/**", "docs/agent-workflow-draft/**", "ops/**", "a.py",
                  "data/usage-journal/**"]
EXCLUDED_PARTS = {"node_modules", "target", "dist", "__pycache__",
                  "playwright-report", "test-results", ".codex-workflow", ".codex",
                  ".codebase-memory", ".memory", ".claude", ".run",
                  ".codex-runtime", ".codex-tmp"}


def git(*args: str) -> bytes:
    return subprocess.check_output(["git", "-C", str(ROOT), *args])


def snapshot() -> dict:
    paths = git("ls-files", "-z", "--cached", "--others", "--exclude-standard",
                "--", ".", *(f":(exclude){p}" for p in INPUT_EXCLUDES)).decode("utf-8").split("\0")
    files = {}
    for relative in sorted(set(filter(None, paths))):
        path = ROOT / relative
        if (path.is_symlink() or not path.is_file()
                or EXCLUDED_PARTS.intersection(Path(relative).parts)
                or path.suffix.lower() in {".md", ".markdown", ".mdx", ".token", ".pem", ".key",
                                          ".ready", ".quarantine"}
                or path.name in {"journal.wal", "journal.lock"}
                or (path.name.startswith(".env") and path.name != ".env.example")
                or relative in {"config.yml", "smoke_config.yml"}):
            continue
        files[relative] = hashlib.sha256(path.read_bytes()).hexdigest()
    if not files:
        raise ValueError("No project inputs found; refusing an empty freshness record")
    encoded = json.dumps(files, sort_keys=True, separators=(",", ":")).encode()
    return {"root": str(ROOT.resolve()), "head": git("rev-parse", "HEAD").decode().strip(),
            "input_sha256": hashlib.sha256(encoded).hexdigest(), "files": files}


def settings() -> tuple[dict, dict]:
    config = json.loads((CACHE / "client.json").read_text(encoding="utf-8"))
    for key in ("command", "runtime_dir", "cache_dir", "project"):
        if not isinstance(config.get(key), str) or not config[key].strip():
            raise ValueError(f"Missing local setting: {key}")
    if not Path(config["command"]).is_file():
        raise ValueError("Configured native graph executable does not exist")
    if config.get("root") != str(ROOT.resolve()):
        raise ValueError("Local graph settings belong to a different worktree")
    environment = dict(os.environ, CBM_RUNTIME_DIR=config["runtime_dir"],
                       CBM_CACHE_DIR=config["cache_dir"], CBM_ALLOWED_ROOT=str(ROOT))
    return config, environment


def invoke(tool: str, arguments: dict, config: dict, environment: dict) -> int:
    arguments = dict(arguments)
    if tool == "index_repository":
        arguments.update(repo_path=str(ROOT), name=config["project"])
    else:
        # Do not allow a task's arguments to silently select another graph.
        if arguments.get("project", config["project"]) != config["project"]:
            raise ValueError("Query project differs from this worktree's index")
        arguments["project"] = config["project"]
    CACHE.mkdir(exist_ok=True)
    with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", suffix=".json",
                                     dir=CACHE, delete=False) as handle:
        json.dump(arguments, handle)
        argument_file = Path(handle.name)
    try:
        return subprocess.run([config["command"], "cli", tool, "--args-file",
                               str(argument_file)], cwd=ROOT, env=environment).returncode
    finally:
        argument_file.unlink(missing_ok=True)


def check_graph_identity(config: dict) -> dict:
    recorded = json.loads((CACHE / "freshness.json").read_text(encoding="utf-8"))
    artifact = json.loads((CACHE / "artifact.json").read_text(encoding="utf-8"))
    if recorded.get("project") != config["project"] or artifact.get("project") != config["project"]:
        raise ValueError("Graph project identity does not match local settings")
    if recorded.get("indexed_at") != artifact.get("indexed_at"):
        raise ValueError("Graph generation differs from freshness record")
    if recorded.get("graph_sha256") != hashlib.sha256((CACHE / "graph.db.zst").read_bytes()).hexdigest():
        raise ValueError("Graph content differs from freshness record")
    return recorded


def check_freshness(report: bool = True) -> dict:
    config, _ = settings()
    recorded = check_graph_identity(config)
    current = snapshot()
    if recorded.get("root") != current["root"]:
        raise ValueError("Freshness record belongs to another worktree")
    if recorded.get("input_sha256") != current["input_sha256"]:
        old = recorded.get("files", {})
        changed = [p for p in sorted(old.keys() | current["files"].keys())
                   if old.get(p) != current["files"].get(p)]
        raise ValueError(f"STALE: {len(changed)} changed inputs; examples: {changed[:5]}")
    if report:
        print(f"Freshness PASS: {len(current['files'])} input files; "
              f"{current['input_sha256']}; HEAD {current['head']}", flush=True)
    return current


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="action", required=True)
    commands.add_parser("refresh", help="Explicit full index; write a fresh content manifest")
    commands.add_parser("status", help="Graph status and local content freshness")
    commands.add_parser("check", help="Fail if product inputs changed since refresh")
    query = commands.add_parser("query", help="Read-only graph tool")
    query.add_argument("tool", choices=sorted(READ_TOOLS))
    query.add_argument("--args-file", type=Path, required=True)
    args = parser.parse_args()
    if args.action == "check":
        check_freshness()
        return 0
    config, environment = settings()
    if args.action == "refresh":
        before = snapshot()
        result = invoke("index_repository", {"mode": "full", "persistence": True},
                        config, environment)
        if result:
            return result
        after = snapshot()
        if before["input_sha256"] != after["input_sha256"]:
            raise ValueError("Inputs changed during indexing; no fresh record written")
        artifact = json.loads((CACHE / "artifact.json").read_text(encoding="utf-8"))
        if artifact.get("project") != config["project"]:
            raise ValueError("Persisted graph has unexpected project identity")
        after.update(project=config["project"], indexed_at=artifact.get("indexed_at"),
                     graph_sha256=hashlib.sha256((CACHE / "graph.db.zst").read_bytes()).hexdigest())
        temporary = CACHE / "freshness.json.tmp"
        temporary.write_text(json.dumps(after, indent=2), encoding="utf-8")
        temporary.replace(CACHE / "freshness.json")
        return 0
    if args.action == "status":
        result = invoke("index_status", {"format": "json", "diagnostics": "summary"},
                        config, environment)
        check_freshness()
        return result
    check_freshness(report=False)
    arguments = json.loads(args.args_file.read_text(encoding="utf-8-sig"))
    if not isinstance(arguments, dict):
        raise ValueError("Tool arguments must be a JSON object")
    return invoke(args.tool, arguments, config, environment)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"Knowledge check FAIL: {error}", file=sys.stderr)
        raise SystemExit(1)
