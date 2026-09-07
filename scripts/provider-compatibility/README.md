# Provider compatibility probe

This opt-in probe exercises an already configured Conduit API deployment from
the public `/v1` boundary through a real upstream provider. It checks model
listing plus non-streaming and streaming Chat Completions and Responses calls.
Every generation uses a fixed, synthetic prompt and a small output limit. The
probe reports only check names and HTTP status codes; it never prints keys,
prompts, or response bodies.

Run it locally only against a deployment and provider account intended for
testing:

```sh
export CONDUIT_TEST_REAL_PROVIDER=1
export CONDUIT_COMPAT_GATEWAY_URL='https://conduit.example/v1'
export CONDUIT_COMPAT_GATEWAY_API_KEY='replace-with-test-project-key'
export CONDUIT_COMPAT_MODEL='replace-with-routed-model'
export CONDUIT_COMPAT_PROTOCOLS='chat,responses'
node scripts/provider-compatibility/provider-smoke.mjs
```

Remote targets must use HTTPS. Loopback HTTP is accepted for local testing.
Set `CONDUIT_COMPAT_PROTOCOLS=chat` when a deliberately limited route does not
offer the Responses API.

The weekly GitHub workflow is disabled until the repository variable
`CONDUIT_REAL_PROVIDER_ENABLED` is set to `1`. Configure these values in the
`provider-compatibility` GitHub environment:

- secret `CONDUIT_COMPAT_GATEWAY_URL`
- secret `CONDUIT_COMPAT_GATEWAY_API_KEY`
- variable `CONDUIT_COMPAT_MODEL`
- variable `CONDUIT_COMPAT_PROTOCOLS` (`chat,responses` is recommended)

The provider account may incur a small real charge on every scheduled run.
Use a dedicated project key with a strict provider-side spending limit.
