# Conduit API

> 感谢Linxu.do支持,学AI,上L站!!!以及任何bug请让Codex小姐背锅

[English](README.md) | [简体中文](README_CN.md)

[![Release gates](https://github.com/404F0X/Conduit-API/actions/workflows/release-gates.yml/badge.svg)](https://github.com/404F0X/Conduit-API/actions/workflows/release-gates.yml)
[![CodeQL](https://github.com/404F0X/Conduit-API/actions/workflows/codeql.yml/badge.svg)](https://github.com/404F0X/Conduit-API/actions/workflows/codeql.yml)

Conduit API is a self-hosted AI gateway for connecting upstream providers,
routing models, managing access, and tracking usage and billing from one web
console.

> [!WARNING]
> Conduit API is currently alpha software. Back up PostgreSQL before upgrades
> and evaluate new versions before using them for production traffic.

## Features

- OpenAI-compatible, Anthropic, Gemini, Jina, Doubao, and AI SDK protocol
  translation.
- Health-aware provider routing, retries, concurrency limits, and prompt-cache
  affinity.
- Provider model discovery, mapping rules, and reviewable pricing updates.
- Projects, scoped API keys, usage accounting, wallets, subscriptions, and
  reusable redemption codes.
- OIDC login, audit history, encrypted configuration backups, and optional
  Redis support.
- A React administration console with English and Simplified Chinese
  interfaces.

## Quick Start

You need Git, Docker, and Docker Compose. The default deployment listens only
on `127.0.0.1:8090` and stores data in a persistent PostgreSQL volume.

For Bash-compatible shells:

```sh
git clone https://github.com/404F0X/Conduit-API.git
cd Conduit-API
export CONDUIT_POSTGRES_PASSWORD='replace-with-a-long-random-value'
docker compose config --quiet
docker compose up --build -d
curl -fsS http://127.0.0.1:8090/health
```

Use only letters, digits, `-`, `.`, `_`, or `~` in this password because the
Compose file inserts it into a PostgreSQL connection URL.

On Windows PowerShell:

```powershell
git clone https://github.com/404F0X/Conduit-API.git
Set-Location Conduit-API
$env:CONDUIT_POSTGRES_PASSWORD = 'replace-with-a-long-random-value'
docker compose config --quiet
docker compose up --build -d
Invoke-WebRequest http://127.0.0.1:8090/health | Out-Null
```

Then open <http://127.0.0.1:8090>. There is no default administrator
password. The first visitor creates the owner account and chooses the actual
accounting currency and the name shown for site credits. Review those values
carefully during initialization.

## Start Using Conduit API

1. Add an upstream provider and its credentials in the administration
   console.
2. Discover or create models, then configure model mappings and routing.
3. Create a project and issue a project API key.
4. Point an OpenAI-compatible client at `http://127.0.0.1:8090/v1` and use the
   project API key as its bearer token.

Users can view their balance and redeem credit codes from the **Wallet** page.
Administrators can create, limit, and revoke redemption codes from
**Billing**.

## Downloads and Deployment

Prebuilt Linux and Windows archives and container images are listed on the
[GitHub Releases](https://github.com/404F0X/Conduit-API/releases) page. Verify a
download with the accompanying `SHA256SUMS` file before running it.

Before exposing Conduit API to the internet, read the
[Production Deployment Guide](https://github.com/404F0X/Conduit-API/blob/main/docs/production-deployment.md).
Use TLS, set the public URL and allowed browser origins, keep secrets outside
the repository, and maintain tested PostgreSQL backups.

## Help and Project Information

- [Production deployment](https://github.com/404F0X/Conduit-API/blob/main/docs/production-deployment.md)
- [Security policy](https://github.com/404F0X/Conduit-API/blob/main/SECURITY.md)
- [Roadmap](https://github.com/404F0X/Conduit-API/blob/main/ROADMAP.md)
- [Contributing](https://github.com/404F0X/Conduit-API/blob/main/CONTRIBUTING.md)
- [Release and build details](RELEASE_GATES.md)
- [Rebuilding and relinking](RELINKING.md)

Please report reproducible bugs through
[GitHub Issues](https://github.com/404F0X/Conduit-API/issues). Do not include
passwords, API keys, provider responses, prompts, or other secrets in an issue.

## License

Most of the repository is licensed under Apache-2.0. The protocol core crates
listed in [LICENSE](LICENSE) are licensed under LGPL-3.0-only. Required
attributions are in [NOTICE](NOTICE) and [frontend/NOTICE](frontend/NOTICE).
