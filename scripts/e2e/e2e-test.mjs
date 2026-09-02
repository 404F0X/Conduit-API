#!/usr/bin/env node

import { createServer as createHttpServer } from 'node:http'
import { createServer as createNetServer } from 'node:net'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const scriptDirectory = dirname(fileURLToPath(import.meta.url))
const repositoryRoot = resolve(scriptDirectory, '..', '..')
const frontendDirectory = join(repositoryRoot, 'frontend')
const configPath = join(repositoryRoot, 'config.example.yml')
const defaultDatabaseDsn =
  'postgresql://conduit:conduit@127.0.0.1:5432/conduit_e2e'
const safeDatabaseName = /^conduit_e2e(?:_[a-z0-9][a-z0-9_]*)?$/
const loopbackHosts = new Set(['127.0.0.1', 'localhost', '::1', '[::1]'])
const externalUrlPattern = /https?:\/\/(?!127\.0\.0\.1(?::|\/)|localhost(?::|\/)|\[::1\](?::|\/))/i

function usage() {
  return `Conduit API isolated Playwright harness

Usage:
  pnpm --dir frontend test:e2e [-- <playwright arguments>]
  pnpm --dir frontend test:e2e:check

Harness options:
  --check       Validate tools, ports, database target, and test isolation only.
                This mode never creates or drops a database or starts a server.
  --keep-db     Keep the isolated E2E database after the run for diagnostics.
  -h, --help    Show this help.

All other arguments are forwarded to "playwright test". Examples:
  pnpm --dir frontend test:e2e -- --project=setup
  pnpm --dir frontend test:e2e -- --headed

Environment:
  CONDUIT_E2E_POSTGRES_DSN      Dedicated PostgreSQL DSN. The database name must
                                match conduit_e2e or conduit_e2e_<suffix>, and
                                the host must be loopback. Default:
                                postgresql://conduit:***@127.0.0.1:5432/conduit_e2e
  CONDUIT_E2E_BACKEND_PORT      Backend port (default: 8099).
  CONDUIT_E2E_FRONTEND_PORT     Vite port (default: 9527).
  CONDUIT_E2E_MOCK_PORT         Local mock upstream port (default: 18099).
  CONDUIT_E2E_BACKEND_TIMEOUT_MS  Backend startup timeout (default: 300000).

Safety:
  The selected database is dropped and recreated before each normal run, then
  dropped again on exit unless --keep-db is used. Real-provider test mode and
  inherited Conduit/provider credentials are removed from child environments.
`
}

function parseArguments(arguments_) {
  const options = { check: false, keepDatabase: false, help: false }
  const playwrightArguments = []

  for (const argument of arguments_) {
    if (argument === '--check') {
      options.check = true
    } else if (argument === '--keep-db') {
      options.keepDatabase = true
    } else if (argument === '--help' || argument === '-h') {
      options.help = true
    } else {
      playwrightArguments.push(argument)
    }
  }

  return { options, playwrightArguments }
}

function readPort(name, fallback) {
  const raw = process.env[name] ?? String(fallback)
  const value = Number.parseInt(raw, 10)
  if (!Number.isInteger(value) || String(value) !== raw || value < 1024 || value > 65535) {
    throw new Error(`${name} must be an integer between 1024 and 65535`)
  }
  return value
}

function readTimeout() {
  const raw = process.env.CONDUIT_E2E_BACKEND_TIMEOUT_MS ?? '300000'
  const value = Number.parseInt(raw, 10)
  if (!Number.isInteger(value) || String(value) !== raw || value < 1000 || value > 900000) {
    throw new Error('CONDUIT_E2E_BACKEND_TIMEOUT_MS must be between 1000 and 900000')
  }
  return value
}

function parseDatabaseDsn(rawDsn) {
  let parsed
  try {
    parsed = new URL(rawDsn)
  } catch {
    throw new Error('CONDUIT_E2E_POSTGRES_DSN must be a valid PostgreSQL URL')
  }

  if (parsed.protocol !== 'postgresql:' && parsed.protocol !== 'postgres:') {
    throw new Error('CONDUIT_E2E_POSTGRES_DSN must use postgresql:// or postgres://')
  }
  if (!loopbackHosts.has(parsed.hostname)) {
    throw new Error(
      `E2E PostgreSQL must be loopback-only; received host ${parsed.hostname || '<empty>'}`,
    )
  }
  if (!parsed.username) {
    throw new Error('CONDUIT_E2E_POSTGRES_DSN must include a PostgreSQL user')
  }
  for (const key of parsed.searchParams.keys()) {
    if (key !== 'sslmode') {
      throw new Error(
        `CONDUIT_E2E_POSTGRES_DSN query parameter "${key}" is not allowed; only sslmode is supported`,
      )
    }
  }
  if (parsed.hash) {
    throw new Error('CONDUIT_E2E_POSTGRES_DSN must not contain a URL fragment')
  }

  const databaseName = decodeURIComponent(parsed.pathname.replace(/^\//, ''))
  if (!safeDatabaseName.test(databaseName)) {
    throw new Error(
      `refusing destructive E2E database operation: database name "${databaseName}" must match ${safeDatabaseName}`,
    )
  }
  if (/(?:^|_)(?:prod|production|live|staging)(?:_|$)/.test(databaseName)) {
    throw new Error(`refusing an environment-like E2E database name: "${databaseName}"`)
  }

  const port = parsed.port || '5432'
  const pgEnvironment = {
    PGHOST: parsed.hostname.replace(/^\[(.*)\]$/, '$1'),
    PGPORT: port,
    PGUSER: decodeURIComponent(parsed.username),
    PGPASSWORD: decodeURIComponent(parsed.password),
    PGDATABASE: databaseName,
  }
  const sslMode = parsed.searchParams.get('sslmode')
  if (sslMode) pgEnvironment.PGSSLMODE = sslMode

  return { rawDsn, databaseName, host: parsed.hostname, port, pgEnvironment }
}

function loadConfiguration() {
  const backendPort = readPort('CONDUIT_E2E_BACKEND_PORT', 8099)
  const frontendPort = readPort('CONDUIT_E2E_FRONTEND_PORT', 9527)
  const mockPort = readPort('CONDUIT_E2E_MOCK_PORT', 18099)
  if (new Set([backendPort, frontendPort, mockPort]).size !== 3) {
    throw new Error('backend, frontend, and mock ports must be distinct')
  }

  const database = parseDatabaseDsn(
    process.env.CONDUIT_E2E_POSTGRES_DSN ?? defaultDatabaseDsn,
  )
  return {
    backendPort,
    frontendPort,
    mockPort,
    backendTimeout: readTimeout(),
    backendUrl: `http://127.0.0.1:${backendPort}`,
    frontendUrl: `http://127.0.0.1:${frontendPort}`,
    mockOrigin: `http://127.0.0.1:${mockPort}`,
    mockUpstreamUrl: `http://127.0.0.1:${mockPort}/v1`,
    database,
  }
}

function pnpmCliPath() {
  const pathEntries = (process.env.PATH ?? '').split(process.platform === 'win32' ? ';' : ':')
  const roots = new Set([
    dirname(process.execPath),
    process.env.PNPM_HOME,
    ...pathEntries,
  ])
  for (const root of roots) {
    if (!root) continue
    for (const relativePath of [
      join('node_modules', 'pnpm', 'bin', 'pnpm.cjs'),
      join('node_modules', 'corepack', 'dist', 'pnpm.js'),
    ]) {
      const candidate = join(root, relativePath)
      if (existsSync(candidate)) return candidate
    }
  }
  return undefined
}

function invocation(command, arguments_) {
  if (command === 'node') return { command: process.execPath, arguments: arguments_ }
  if (command === 'pnpm' && process.platform === 'win32') {
    const cliPath = pnpmCliPath()
    if (!cliPath) {
      throw new Error('could not locate pnpm.cjs behind the Windows pnpm shim')
    }
    return { command: process.execPath, arguments: [cliPath, ...arguments_] }
  }
  return { command, arguments: arguments_ }
}

function sanitizeEnvironment() {
  const environment = {}
  const secretName = /(?:API_?KEY|TOKEN|SECRET|CREDENTIAL|PRIVATE_?KEY|ACCESS_?KEY|AUTHORIZATION|PASSWORD)/i

  for (const [name, value] of Object.entries(process.env)) {
    const upperName = name.toUpperCase()
    if (
      value === undefined ||
      upperName.startsWith('CONDUIT_') ||
      upperName.startsWith('VITE_') ||
      /^(?:HTTP|HTTPS|ALL|NO)_PROXY$/.test(upperName) ||
      secretName.test(name)
    ) {
      continue
    }
    environment[name] = value
  }
  return environment
}

function backendEnvironment(configuration) {
  return {
    ...sanitizeEnvironment(),
    CONDUIT_DB_DIALECT: 'postgres',
    CONDUIT_DB_DSN: configuration.database.rawDsn,
    CONDUIT_DB_MAX_OPEN_CONNS: '10',
    CONDUIT_DB_MAX_IDLE_CONNS: '5',
    CONDUIT_SERVER_HOST: '127.0.0.1',
    CONDUIT_SERVER_PORT: String(configuration.backendPort),
    CONDUIT_SERVER_PUBLIC_URL: configuration.backendUrl,
    CONDUIT_SERVER_BASE_PATH: '',
    CONDUIT_SERVER_TRUSTED_PROXIES: '[]',
    CONDUIT_SERVER_CORS_ALLOWED_ORIGINS: JSON.stringify([
      configuration.frontendUrl,
      `http://localhost:${configuration.frontendPort}`,
    ]),
    CONDUIT_METRICS_ENABLED: 'false',
    CONDUIT_GC_ENABLED: 'false',
    CONDUIT_PROVIDER_QUOTA_ENABLED: 'false',
    CONDUIT_OIDC_ENABLED: 'false',
    CONDUIT_OIDC_PROVIDERS: '[]',
    CONDUIT_CACHE_ROUTE_AFFINITY_ENABLED: 'false',
    CONDUIT_API_AUTH_BCRYPT_COST: '4',
    CONDUIT_LOG_LEVEL: 'warn',
    CONDUIT_LOG_STDOUT: 'true',
    CONDUIT_TEST_REAL_PROVIDER: '0',
    CARGO_NET_OFFLINE: 'true',
    HTTP_PROXY: configuration.mockOrigin,
    HTTPS_PROXY: configuration.mockOrigin,
    ALL_PROXY: configuration.mockOrigin,
    NO_PROXY: '127.0.0.1,localhost,::1',
    http_proxy: configuration.mockOrigin,
    https_proxy: configuration.mockOrigin,
    all_proxy: configuration.mockOrigin,
    no_proxy: '127.0.0.1,localhost,::1',
  }
}

function playwrightEnvironment(configuration) {
  return {
    ...sanitizeEnvironment(),
    CONDUIT_ADMIN_EMAIL: 'e2e-owner@conduit.invalid',
    CONDUIT_ADMIN_PASSWORD: 'conduit-e2e-password-2026',
    CONDUIT_API_URL: configuration.backendUrl,
    CONDUIT_E2E_FRONTEND_URL: configuration.frontendUrl,
    CONDUIT_E2E_FRONTEND_PORT: String(configuration.frontendPort),
    CONDUIT_E2E_MOCK_ORIGIN: configuration.mockOrigin,
    CONDUIT_E2E_MOCK_UPSTREAM_URL: configuration.mockUpstreamUrl,
    CONDUIT_TEST_REAL_PROVIDER: '0',
    VITE_API_URL: configuration.backendUrl,
    VITE_PORT: String(configuration.frontendPort),
  }
}

function postgresEnvironment(configuration) {
  return {
    ...sanitizeEnvironment(),
    ...configuration.database.pgEnvironment,
  }
}

function run(command, arguments_, options = {}) {
  return new Promise((resolvePromise, rejectPromise) => {
    const resolved = invocation(command, arguments_)
    const child = spawn(resolved.command, resolved.arguments, {
      cwd: options.cwd ?? repositoryRoot,
      env: options.env ?? sanitizeEnvironment(),
      stdio: options.stdio ?? 'inherit',
      windowsHide: true,
    })
    child.once('error', rejectPromise)
    child.once('exit', (code, signal) => {
      if (code === 0) {
        resolvePromise()
      } else {
        rejectPromise(
          new Error(
            `${command} ${arguments_.join(' ')} failed ${signal ? `with ${signal}` : `with exit code ${code}`}`,
          ),
        )
      }
    })
  })
}

function capture(command, arguments_, options = {}) {
  return new Promise((resolvePromise, rejectPromise) => {
    const resolved = invocation(command, arguments_)
    const child = spawn(resolved.command, resolved.arguments, {
      cwd: options.cwd ?? repositoryRoot,
      env: options.env ?? sanitizeEnvironment(),
      stdio: ['ignore', 'pipe', 'pipe'],
      windowsHide: true,
    })
    let stdout = ''
    let stderr = ''
    child.stdout.on('data', (chunk) => (stdout += chunk.toString()))
    child.stderr.on('data', (chunk) => (stderr += chunk.toString()))
    child.once('error', rejectPromise)
    child.once('exit', (code) => {
      if (code === 0) resolvePromise((stdout || stderr).trim())
      else rejectPromise(new Error(`${command} is unavailable or returned exit code ${code}`))
    })
  })
}

async function verifyPrerequisites() {
  if (!existsSync(configPath)) throw new Error(`missing backend configuration: ${configPath}`)
  if (!existsSync(join(frontendDirectory, 'playwright.config.ts'))) {
    throw new Error('missing frontend/playwright.config.ts')
  }

  const checks = [
    ['node', ['--version']],
    ['pnpm', ['--version']],
    ['cargo', ['--version']],
    ['psql', ['--version']],
    ['createdb', ['--version']],
    ['dropdb', ['--version']],
  ]
  const versions = []
  for (const [command, arguments_] of checks) {
    const output = await capture(command, arguments_)
    versions.push(`${command}: ${output.split(/\r?\n/, 1)[0]}`)
  }
  const playwrightVersion = await capture('pnpm', ['exec', 'playwright', '--version'], {
    cwd: frontendDirectory,
  })
  const installedBrowsers = await capture('pnpm', ['exec', 'playwright', 'install', '--list'], {
    cwd: frontendDirectory,
  })
  if (!/chromium/i.test(installedBrowsers)) {
    throw new Error(
      'Playwright Chromium is not installed; run "pnpm --dir frontend exec playwright install chromium"',
    )
  }
  versions.push(`playwright: ${playwrightVersion.split(/\r?\n/, 1)[0]} (Chromium installed)`)
  return versions
}

function allTestFiles(directory) {
  const files = []
  for (const entry of readdirSync(directory)) {
    const path = join(directory, entry)
    if (statSync(path).isDirectory()) files.push(...allTestFiles(path))
    else if (entry.endsWith('.ts')) files.push(path)
  }
  return files
}

function verifyTestNetworkIsolation() {
  const violations = []
  for (const path of allTestFiles(join(frontendDirectory, 'tests'))) {
    const lines = readFileSync(path, 'utf8').split(/\r?\n/)
    lines.forEach((line, index) => {
      if (externalUrlPattern.test(line)) {
        violations.push(`${path}:${index + 1}`)
      }
    })
  }
  if (violations.length > 0) {
    throw new Error(
      `E2E specs contain non-loopback URL literals; route them through the local mock:\n${violations.join('\n')}`,
    )
  }
}

async function verifyPortAvailable(port, label) {
  const server = createNetServer()
  server.unref()
  await new Promise((resolvePromise, rejectPromise) => {
    server.once('error', (error) => {
      rejectPromise(new Error(`${label} port ${port} is unavailable: ${error.message}`))
    })
    server.listen(port, '127.0.0.1', resolvePromise)
  })
  server.close()
  await once(server, 'close')
}

async function verifyPorts(configuration) {
  await verifyPortAvailable(configuration.backendPort, 'backend')
  await verifyPortAvailable(configuration.frontendPort, 'frontend')
  await verifyPortAvailable(configuration.mockPort, 'mock upstream')
}

async function resetDatabase(configuration, onCreated) {
  const environment = postgresEnvironment(configuration)
  const name = configuration.database.databaseName
  await run('dropdb', ['--if-exists', '--force', '--maintenance-db', 'postgres', name], {
    env: environment,
  })
  await run(
    'createdb',
    ['--maintenance-db', 'postgres', '--encoding', 'UTF8', '--template', 'template0', name],
    { env: environment },
  )
  onCreated()
  await run(
    'psql',
    [
      '--no-psqlrc',
      '--dbname',
      name,
      '--tuples-only',
      '--no-align',
      '--command',
      'SELECT current_database()',
    ],
    { env: environment, stdio: ['ignore', 'ignore', 'inherit'] },
  )
}

async function verifyDatabasePrivileges(configuration) {
  const output = await capture(
    'psql',
    [
      '--no-psqlrc',
      '--dbname',
      'postgres',
      '--tuples-only',
      '--no-align',
      '--command',
      "SELECT CASE WHEN rolcreatedb OR rolsuper THEN 'yes' ELSE 'no' END FROM pg_roles WHERE rolname = current_user",
    ],
    { env: postgresEnvironment(configuration) },
  )
  if (output.trim() !== 'yes') {
    throw new Error(
      'the E2E PostgreSQL role needs CREATEDB (or superuser) permission; no database was changed',
    )
  }
}

async function dropDatabase(configuration) {
  const name = configuration.database.databaseName
  if (!safeDatabaseName.test(name)) {
    throw new Error(`refusing to drop unsafe database name: ${name}`)
  }
  await run('dropdb', ['--if-exists', '--force', '--maintenance-db', 'postgres', name], {
    env: postgresEnvironment(configuration),
  })
}

function jsonResponse(response, status, value) {
  const body = JSON.stringify(value)
  response.writeHead(status, {
    'content-type': 'application/json',
    'content-length': Buffer.byteLength(body),
    'cache-control': 'no-store',
  })
  response.end(body)
}

async function startMockUpstream(configuration) {
  const server = createHttpServer((request, response) => {
    const url = new URL(request.url ?? '/', configuration.mockOrigin)
    request.resume()
    response.setHeader('connection', 'close')

    if (!loopbackHosts.has(url.hostname)) {
      jsonResponse(response, 403, { error: 'external network is disabled in Conduit E2E' })
      return
    }

    if (request.method === 'OPTIONS') {
      response.writeHead(204)
      response.end()
      return
    }
    if (url.pathname === '/health') {
      jsonResponse(response, 200, { status: 'ok' })
      return
    }
    if (url.pathname.endsWith('/models')) {
      jsonResponse(response, 200, {
        object: 'list',
        data: [{ id: 'gpt-4o', object: 'model', owned_by: 'conduit-e2e' }],
      })
      return
    }
    if (url.pathname.endsWith('/chat/completions')) {
      jsonResponse(response, 200, {
        id: 'chatcmpl-conduit-e2e',
        object: 'chat.completion',
        created: 0,
        model: 'gpt-4o',
        choices: [
          {
            index: 0,
            message: { role: 'assistant', content: 'local mock response' },
            finish_reason: 'stop',
          },
        ],
        usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
      })
      return
    }
    if (url.pathname.endsWith('/messages')) {
      jsonResponse(response, 200, {
        id: 'msg_conduit_e2e',
        type: 'message',
        role: 'assistant',
        model: 'claude-mock',
        content: [{ type: 'text', text: 'local mock response' }],
        stop_reason: 'end_turn',
        usage: { input_tokens: 1, output_tokens: 1 },
      })
      return
    }
    if (url.pathname.includes(':generateContent')) {
      jsonResponse(response, 200, {
        candidates: [{ content: { role: 'model', parts: [{ text: 'local mock response' }] } }],
        usageMetadata: { promptTokenCount: 1, candidatesTokenCount: 1, totalTokenCount: 2 },
      })
      return
    }

    jsonResponse(response, 200, { ok: true, source: 'conduit-e2e-local-mock' })
  })
  server.on('connect', (_request, socket) => {
    socket.end('HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n')
  })
  server.keepAliveTimeout = 1000
  await new Promise((resolvePromise, rejectPromise) => {
    server.once('error', rejectPromise)
    server.listen(configuration.mockPort, '127.0.0.1', resolvePromise)
  })
  return server
}

function redactLog(value) {
  return value
    .replace(/postgres(?:ql)?:\/\/[^\s"']+/gi, '[REDACTED_POSTGRES_DSN]')
    .replace(/Bearer\s+[^\s"']+/gi, 'Bearer [REDACTED]')
    .replace(/\b(?:sk|key)-[A-Za-z0-9._-]{8,}\b/g, '[REDACTED_TOKEN]')
}

function forwardRedacted(stream, prefix) {
  let buffered = ''
  stream.on('data', (chunk) => {
    buffered += chunk.toString()
    const lines = buffered.split(/\r?\n/)
    buffered = lines.pop() ?? ''
    for (const line of lines) {
      if (line) process.stderr.write(`${prefix}${redactLog(line)}\n`)
    }
  })
  stream.on('end', () => {
    if (buffered) process.stderr.write(`${prefix}${redactLog(buffered)}\n`)
  })
}

function startBackend(configuration) {
  const child = spawn(
    'cargo',
    [
      'run',
      '--locked',
      '--quiet',
      '-p',
      'conduit-bin',
      '--bin',
      'conduit-api',
      '--',
      '--config',
      configPath,
    ],
    {
      cwd: repositoryRoot,
      env: backendEnvironment(configuration),
      stdio: ['ignore', 'pipe', 'pipe'],
      windowsHide: true,
      detached: process.platform !== 'win32',
    },
  )
  child.e2eSpawnError = undefined
  child.on('error', (error) => {
    child.e2eSpawnError = error
  })
  forwardRedacted(child.stdout, '[backend] ')
  forwardRedacted(child.stderr, '[backend] ')
  return child
}

async function waitForBackend(configuration, child) {
  const deadline = Date.now() + configuration.backendTimeout
  const healthUrl = `${configuration.backendUrl}/health`
  let lastError = 'not started'

  while (Date.now() < deadline) {
    if (child.e2eSpawnError) {
      throw new Error(`could not start backend: ${child.e2eSpawnError.message}`)
    }
    if (child.exitCode !== null || child.signalCode !== null) {
      throw new Error(
        `backend exited before becoming healthy (exit=${child.exitCode}, signal=${child.signalCode})`,
      )
    }
    try {
      const response = await fetch(healthUrl, { signal: AbortSignal.timeout(2000) })
      if (response.ok) return
      lastError = `HTTP ${response.status}`
    } catch (error) {
      lastError = error instanceof Error ? error.message : String(error)
    }
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 500))
  }
  throw new Error(`backend did not become healthy within ${configuration.backendTimeout}ms: ${lastError}`)
}

function startPlaywright(configuration, arguments_) {
  const resolved = invocation('pnpm', ['exec', 'playwright', 'test', ...arguments_])
  return spawn(resolved.command, resolved.arguments, {
    cwd: frontendDirectory,
    env: playwrightEnvironment(configuration),
    stdio: 'inherit',
    windowsHide: true,
    detached: process.platform !== 'win32',
  })
}

function waitForChild(child, label) {
  return new Promise((resolvePromise, rejectPromise) => {
    child.once('error', rejectPromise)
    child.once('exit', (code, signal) => {
      if (code === 0) resolvePromise()
      else rejectPromise(new Error(`${label} failed ${signal ? `with ${signal}` : `with exit code ${code}`}`))
    })
  })
}

function waitForExit(child, timeoutMilliseconds) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return Promise.resolve()
  return Promise.race([
    once(child, 'exit').then(() => undefined),
    new Promise((resolvePromise) => setTimeout(resolvePromise, timeoutMilliseconds)),
  ])
}

async function killWindowsTree(processId) {
  await new Promise((resolvePromise) => {
    const killer = spawn('taskkill.exe', ['/PID', String(processId), '/T', '/F'], {
      stdio: 'ignore',
      windowsHide: true,
    })
    killer.once('error', resolvePromise)
    killer.once('exit', resolvePromise)
  })
}

async function stopChild(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null || !child.pid) return
  const processId = child.pid
  if (process.platform === 'win32') {
    // Killing only cargo/pnpm can orphan conduit-api or Vite on Windows.
    await killWindowsTree(processId)
    await waitForExit(child, 8000)
    return
  }
  try {
    process.kill(-processId, 'SIGTERM')
  } catch {
    // The process may have exited between the state check and the signal.
  }
  await waitForExit(child, 8000)
  try {
    // The group may still contain a descendant after cargo/pnpm has exited.
    process.kill(-processId, 'SIGKILL')
  } catch {
    // The complete process group already exited.
  }
}

async function closeServer(server) {
  if (!server) return
  await new Promise((resolvePromise) => server.close(resolvePromise))
}

function printConfiguration(configuration) {
  console.log('E2E isolation configuration:')
  console.log(`  database: ${configuration.database.databaseName}`)
  console.log(`  postgres: ${configuration.database.host}:${configuration.database.port} (credentials redacted)`)
  console.log(`  backend:  ${configuration.backendUrl}`)
  console.log(`  frontend: ${configuration.frontendUrl}`)
  console.log(`  mock:     ${configuration.mockUpstreamUrl}`)
}

async function main() {
  const { options, playwrightArguments } = parseArguments(process.argv.slice(2))
  if (options.help) {
    console.log(usage())
    return
  }

  const configuration = loadConfiguration()
  verifyTestNetworkIsolation()
  printConfiguration(configuration)
  const versions = await verifyPrerequisites()
  await verifyPorts(configuration)

  if (options.check) {
    console.log('Prerequisites:')
    for (const version of versions) console.log(`  ${version}`)
    console.log('E2E configuration check passed; no database or server was touched.')
    return
  }

  const resources = { backend: undefined, playwright: undefined, mock: undefined, database: false }
  let cleanupPromise
  const cleanup = () => {
    if (cleanupPromise) return cleanupPromise
    cleanupPromise = (async () => {
      await stopChild(resources.playwright)
      await stopChild(resources.backend)
      await closeServer(resources.mock)
      if (resources.database && !options.keepDatabase) {
        await dropDatabase(configuration)
        resources.database = false
      }
    })()
    return cleanupPromise
  }

  let interrupted = false
  const onSignal = (signal) => {
    if (interrupted) return
    interrupted = true
    console.error(`Received ${signal}; stopping isolated E2E services...`)
    cleanup()
      .catch((error) => console.error(redactLog(error instanceof Error ? error.message : String(error))))
      .finally(() => process.exit(signal === 'SIGINT' ? 130 : 143))
  }
  process.once('SIGINT', () => onSignal('SIGINT'))
  process.once('SIGTERM', () => onSignal('SIGTERM'))

  try {
    await verifyDatabasePrivileges(configuration)
    console.log(`Resetting isolated PostgreSQL database ${configuration.database.databaseName}...`)
    await resetDatabase(configuration, () => {
      resources.database = true
    })
    resources.mock = await startMockUpstream(configuration)
    resources.backend = startBackend(configuration)
    await waitForBackend(configuration, resources.backend)
    console.log('Backend and local mock are ready; starting Playwright...')
    resources.playwright = startPlaywright(configuration, playwrightArguments)
    await waitForChild(resources.playwright, 'Playwright')
  } finally {
    await cleanup()
    if (options.keepDatabase && resources.database) {
      console.log(`Kept isolated database ${configuration.database.databaseName} (--keep-db).`)
    }
  }
}

main().catch((error) => {
  console.error(`E2E harness failed: ${redactLog(error instanceof Error ? error.message : String(error))}`)
  process.exitCode = 1
})
