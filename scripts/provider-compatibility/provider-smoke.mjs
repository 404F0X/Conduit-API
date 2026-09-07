#!/usr/bin/env node

import { pathToFileURL } from 'node:url'

const MAX_RESPONSE_BYTES = 1024 * 1024
const DEFAULT_TIMEOUT_MS = 45_000

function requiredEnvironment(environment, name) {
  const value = environment[name]?.trim()
  if (!value) throw new Error(`${name} must be set`)
  return value
}

export function configurationFromEnvironment(environment = process.env) {
  if (environment.CONDUIT_TEST_REAL_PROVIDER !== '1') {
    throw new Error('refusing real-provider traffic unless CONDUIT_TEST_REAL_PROVIDER=1')
  }
  const rawUrl = requiredEnvironment(environment, 'CONDUIT_COMPAT_GATEWAY_URL')
  const gatewayUrl = new URL(rawUrl)
  const loopback = ['127.0.0.1', 'localhost', '::1', '[::1]'].includes(gatewayUrl.hostname)
  if (gatewayUrl.protocol !== 'https:' && !(gatewayUrl.protocol === 'http:' && loopback)) {
    throw new Error('CONDUIT_COMPAT_GATEWAY_URL must use HTTPS unless it is loopback')
  }
  if (gatewayUrl.username || gatewayUrl.password || gatewayUrl.search || gatewayUrl.hash) {
    throw new Error('CONDUIT_COMPAT_GATEWAY_URL must not contain credentials, query parameters, or fragments')
  }
  gatewayUrl.pathname = `${gatewayUrl.pathname.replace(/\/+$/, '')}/`

  const protocols = new Set(
    (environment.CONDUIT_COMPAT_PROTOCOLS ?? 'chat,responses')
      .split(',')
      .map((value) => value.trim().toLowerCase())
      .filter(Boolean),
  )
  for (const protocol of protocols) {
    if (!['chat', 'responses'].includes(protocol)) {
      throw new Error(`unsupported CONDUIT_COMPAT_PROTOCOLS entry: ${protocol}`)
    }
  }
  if (protocols.size === 0) throw new Error('CONDUIT_COMPAT_PROTOCOLS must select at least one protocol')

  const timeoutMs = Number.parseInt(environment.CONDUIT_COMPAT_TIMEOUT_MS ?? `${DEFAULT_TIMEOUT_MS}`, 10)
  if (!Number.isInteger(timeoutMs) || timeoutMs < 1_000 || timeoutMs > 120_000) {
    throw new Error('CONDUIT_COMPAT_TIMEOUT_MS must be an integer between 1000 and 120000')
  }
  return {
    gatewayUrl,
    apiKey: requiredEnvironment(environment, 'CONDUIT_COMPAT_GATEWAY_API_KEY'),
    model: requiredEnvironment(environment, 'CONDUIT_COMPAT_MODEL'),
    protocols,
    timeoutMs,
  }
}

function endpoint(configuration, path) {
  return new URL(path.replace(/^\//, ''), configuration.gatewayUrl)
}

async function readBoundedText(response) {
  const declared = Number.parseInt(response.headers.get('content-length') ?? '0', 10)
  if (declared > MAX_RESPONSE_BYTES) throw new Error('response exceeded the 1 MiB compatibility limit')

  if (!response.body) return ''
  const chunks = []
  let received = 0
  for await (const chunk of response.body) {
    received += chunk.byteLength
    if (received > MAX_RESPONSE_BYTES) {
      await response.body.cancel().catch(() => {})
      throw new Error('response exceeded the 1 MiB compatibility limit')
    }
    chunks.push(Buffer.from(chunk))
  }
  return Buffer.concat(chunks, received).toString('utf8')
}

async function request(configuration, fetchImplementation, path, init = {}) {
  const response = await fetchImplementation(endpoint(configuration, path), {
    ...init,
    headers: {
      authorization: `Bearer ${configuration.apiKey}`,
      'content-type': 'application/json',
      ...(init.headers ?? {}),
    },
    signal: AbortSignal.timeout(configuration.timeoutMs),
  })
  const text = await readBoundedText(response)
  if (!response.ok) throw new Error(`${path} returned HTTP ${response.status}`)
  return { response, text }
}

function parseJson(path, text) {
  try {
    return JSON.parse(text)
  } catch {
    throw new Error(`${path} returned invalid JSON`)
  }
}

function parseServerSentEvents(path, text) {
  const events = []
  let done = false
  for (const line of text.split(/\r?\n/)) {
    if (!line.startsWith('data:')) continue
    const data = line.slice(5).trim()
    if (!data) continue
    if (data === '[DONE]') {
      done = true
      continue
    }
    events.push(parseJson(path, data))
  }
  if (events.length === 0) throw new Error(`${path} returned no JSON SSE events`)
  return { events, done }
}

async function checkModels(configuration, fetchImplementation) {
  const { text } = await request(configuration, fetchImplementation, 'models', { method: 'GET' })
  const body = parseJson('/models', text)
  if (!Array.isArray(body.data)) throw new Error('/models response must contain a data array')
}

async function checkChat(configuration, fetchImplementation, stream) {
  const path = 'chat/completions'
  const { text } = await request(configuration, fetchImplementation, path, {
    method: 'POST',
    body: JSON.stringify({
      model: configuration.model,
      messages: [{ role: 'user', content: 'Reply with exactly OK.' }],
      max_tokens: 8,
      stream,
    }),
  })
  if (stream) {
    const result = parseServerSentEvents(`/${path}`, text)
    if (!result.done) throw new Error(`/${path} stream did not terminate with [DONE]`)
  } else {
    const body = parseJson(`/${path}`, text)
    if (!Array.isArray(body.choices) || body.choices.length === 0) {
      throw new Error(`/${path} response must contain at least one choice`)
    }
  }
}

async function checkResponses(configuration, fetchImplementation, stream) {
  const path = 'responses'
  const { text } = await request(configuration, fetchImplementation, path, {
    method: 'POST',
    body: JSON.stringify({
      model: configuration.model,
      input: 'Reply with exactly OK.',
      max_output_tokens: 8,
      stream,
    }),
  })
  if (stream) {
    const { events } = parseServerSentEvents(`/${path}`, text)
    if (!events.some((event) => event.type === 'response.completed')) {
      throw new Error(`/${path} stream did not contain response.completed`)
    }
  } else {
    const body = parseJson(`/${path}`, text)
    if (typeof body.id !== 'string' || !Array.isArray(body.output)) {
      throw new Error(`/${path} response must contain an id and output array`)
    }
  }
}

export async function runCompatibilitySuite(
  configuration,
  { fetchImplementation = fetch, log = console.log } = {},
) {
  const checks = [['models', () => checkModels(configuration, fetchImplementation)]]
  if (configuration.protocols.has('chat')) {
    checks.push(
      ['chat non-stream', () => checkChat(configuration, fetchImplementation, false)],
      ['chat stream', () => checkChat(configuration, fetchImplementation, true)],
    )
  }
  if (configuration.protocols.has('responses')) {
    checks.push(
      ['responses non-stream', () => checkResponses(configuration, fetchImplementation, false)],
      ['responses stream', () => checkResponses(configuration, fetchImplementation, true)],
    )
  }

  for (const [name, check] of checks) {
    await check()
    log(`PASS ${name}`)
  }
  log(`Provider compatibility passed for ${configuration.gatewayUrl.origin}`)
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  runCompatibilitySuite(configurationFromEnvironment()).catch((error) => {
    console.error(`Provider compatibility failed: ${error instanceof Error ? error.message : String(error)}`)
    process.exitCode = 1
  })
}
