import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import test from 'node:test'
import { configurationFromEnvironment, runCompatibilitySuite } from './provider-smoke.mjs'

function json(response, value) {
  const body = JSON.stringify(value)
  response.writeHead(200, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(body) })
  response.end(body)
}

async function withMockGateway(run) {
  const requests = []
  const server = createServer((request, response) => {
    let body = ''
    request.on('data', (chunk) => (body += chunk.toString()))
    request.on('end', () => {
      requests.push({ method: request.method, url: request.url, authorization: request.headers.authorization, body })
      if (request.url === '/v1/models') {
        json(response, { object: 'list', data: [{ id: 'test-model', object: 'model' }] })
        return
      }
      const input = JSON.parse(body)
      if (request.url === '/v1/chat/completions' && input.stream) {
        response.writeHead(200, { 'content-type': 'text/event-stream' })
        response.end('data: {"choices":[{"delta":{"content":"OK"}}]}\n\ndata: [DONE]\n\n')
        return
      }
      if (request.url === '/v1/chat/completions') {
        json(response, { id: 'chat-test', choices: [{ message: { role: 'assistant', content: 'OK' } }] })
        return
      }
      if (request.url === '/v1/responses' && input.stream) {
        response.writeHead(200, { 'content-type': 'text/event-stream' })
        response.end('data: {"type":"response.output_text.delta","delta":"OK"}\n\ndata: {"type":"response.completed"}\n\n')
        return
      }
      if (request.url === '/v1/responses') {
        json(response, { id: 'resp-test', output: [{ type: 'message' }] })
        return
      }
      response.writeHead(404).end()
    })
  })
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  try {
    const address = server.address()
    if (!address || typeof address === 'string') throw new Error('mock gateway did not expose a TCP address')
    await run(`http://127.0.0.1:${address.port}/v1`, requests)
  } finally {
    await new Promise((resolve) => server.close(resolve))
  }
}

test('real-provider mode is explicit and rejects insecure remote URLs', () => {
  assert.throws(() => configurationFromEnvironment({}), /CONDUIT_TEST_REAL_PROVIDER=1/)
  assert.throws(
    () =>
      configurationFromEnvironment({
        CONDUIT_TEST_REAL_PROVIDER: '1',
        CONDUIT_COMPAT_GATEWAY_URL: 'http://provider.example/v1',
        CONDUIT_COMPAT_GATEWAY_API_KEY: 'secret-test-key',
        CONDUIT_COMPAT_MODEL: 'test-model',
      }),
    /must use HTTPS/,
  )
})

test('compatibility suite checks models plus streaming and non-streaming protocols without logging secrets', async () => {
  await withMockGateway(async (gatewayUrl, requests) => {
    const configuration = configurationFromEnvironment({
      CONDUIT_TEST_REAL_PROVIDER: '1',
      CONDUIT_COMPAT_GATEWAY_URL: gatewayUrl,
      CONDUIT_COMPAT_GATEWAY_API_KEY: 'secret-test-key',
      CONDUIT_COMPAT_MODEL: 'test-model',
    })
    const logs = []
    await runCompatibilitySuite(configuration, { log: (line) => logs.push(line) })

    assert.deepEqual(
      requests.map((request) => `${request.method} ${request.url}`),
      [
        'GET /v1/models',
        'POST /v1/chat/completions',
        'POST /v1/chat/completions',
        'POST /v1/responses',
        'POST /v1/responses',
      ],
    )
    assert.ok(requests.every((request) => request.authorization === 'Bearer secret-test-key'))
    assert.ok(logs.every((line) => !line.includes('secret-test-key') && !line.includes('Reply with exactly OK.')))
    assert.match(logs.at(-1), /Provider compatibility passed/)
  })
})

test('compatibility suite stops reading responses above its memory limit', async () => {
  const configuration = {
    gatewayUrl: new URL('https://conduit.example/v1/'),
    apiKey: 'secret-test-key',
    model: 'test-model',
    protocols: new Set(),
    timeoutMs: 1_000,
  }
  const oversizedBody = new Uint8Array(1024 * 1024 + 1)

  await assert.rejects(
    runCompatibilitySuite(configuration, {
      fetchImplementation: async () => new Response(oversizedBody),
      log: () => {},
    }),
    /response exceeded the 1 MiB compatibility limit/,
  )
})
