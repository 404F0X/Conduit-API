// Fixed maajiko/Epay a4d0f042 reference fixture, not a real merchant gateway.
import { createHash, createPublicKey, timingSafeEqual } from 'node:crypto'
import { spawnSync } from 'node:child_process'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { createServer, request as httpsRequest } from 'node:https'
import { request as httpRequest } from 'node:http'
import { join } from 'node:path'

export const FIXTURE_MERCHANT = '10001'
export const FIXTURE_KEY = 'isolated-reference-fixture-key'

export function signFixtureParams(params, key = FIXTURE_KEY) {
  const entries = Object.entries(params)
    .filter(([name, value]) => name !== 'sign' && name !== 'sign_type' && value != null && !Array.isArray(value) && String(value).trim() !== '')
    .sort(([a], [b]) => Buffer.compare(Buffer.from(a), Buffer.from(b)))
  return createHash('md5').update(entries.map(([name, value]) => `${name}=${value}`).join('&') + key, 'utf8').digest('hex')
}

export function parseFixtureQuery(raw) {
  if (raw.length > 16384 || /%(?![a-f\d]{2})/i.test(raw)) throw new Error('Invalid fixture query')
  const result = Object.create(null)
  for (const part of raw ? raw.split('&') : []) {
    const separator = part.indexOf('=')
    const decode = (value) => decodeURIComponent(value.replaceAll('+', ' '))
    const name = decode(separator < 0 ? part : part.slice(0, separator))
    const value = decode(separator < 0 ? '' : part.slice(separator + 1))
    if (!/^[A-Za-z_][A-Za-z_\d]*$/.test(name) || Object.hasOwn(result, name) || Object.keys(result).length >= 32) throw new Error('Invalid fixture parameter')
    result[name] = value
  }
  return result
}

export function preparePaymentTLS(directory, environment) {
  const path = join(directory, 'payment-tls')
  mkdirSync(path, { mode: 0o700 })
  writeFileSync(join(path, 'openssl.cnf'), '[req]\ndistinguished_name=dn\n[dn]\n', { mode: 0o600 })
  const run = (args) => {
    const result = spawnSync('openssl', args, { cwd: path, env: environment, windowsHide: true, encoding: 'utf8', timeout: 30000 })
    if (result.status !== 0) throw new Error(`Could not generate owned payment fixture TLS certificate (${args[0]}, status=${result.status}, code=${result.error?.code ?? 'none'}): ${result.stderr?.slice(-1600) ?? ''}`)
  }
  run(['req', '-config', 'openssl.cnf', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', 'ca.key', '-out', 'ca.pem', '-days', '1', '-subj', '/CN=Conduit isolated fixture CA', '-addext', 'basicConstraints=critical,CA:TRUE'])
  run(['req', '-config', 'openssl.cnf', '-new', '-newkey', 'rsa:2048', '-nodes', '-keyout', 'server.key', '-out', 'server.csr', '-subj', '/CN=127.0.0.1'])
  writeFileSync(join(path, 'server.ext'), 'subjectAltName=IP:127.0.0.1,DNS:localhost\nbasicConstraints=critical,CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n', { mode: 0o600 })
  run(['x509', '-req', '-in', 'server.csr', '-CA', 'ca.pem', '-CAkey', 'ca.key', '-CAcreateserial', '-out', 'server.pem', '-days', '1', '-extfile', 'server.ext'])
  const key = readFileSync(join(path, 'server.key'))
  const cert = readFileSync(join(path, 'server.pem'))
  const ca = readFileSync(join(path, 'ca.pem'), 'utf8')
  const spki = createHash('sha256').update(createPublicKey(key).export({ type: 'spki', format: 'der' })).digest('base64')
  return { key, cert, ca, spki }
}

function html(value) { return String(value).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;').replaceAll("'", '&#39;') }
function verified(params) {
  if (params.pid !== FIXTURE_MERCHANT || params.sign_type !== 'MD5' || !/^[a-f\d]{32}$/.test(params.sign ?? '')) return false
  return timingSafeEqual(Buffer.from(params.sign), Buffer.from(signFixtureParams(params)))
}
async function callback(url, ca) {
  return new Promise((resolve, reject) => {
    const request = httpsRequest(url, { ca, timeout: 10000 }, (response) => {
      let body = ''
      response.on('data', (chunk) => { body += chunk; if (body.length > 1024) response.destroy() })
      response.on('end', () => resolve(response.statusCode === 200 && body === 'success'))
    })
    request.on('timeout', () => request.destroy(new Error('Fixture callback timed out')))
    // Never include an error's URL (which contains a fixture signature).
    request.on('error', () => reject(new Error('Fixture callback unavailable')))
    request.end()
  })
}

export async function startPaymentFixture(configuration, tls) {
  const orders = new Map()
  const origin = configuration.paymentOrigin
  const notifyURL = `${origin}/api/payments/epay/notify`
  const returnURL = `${origin}/return`
  const server = createServer({ key: tls.key, cert: tls.cert }, async (request, response) => {
    try {
      const url = new URL(request.url, origin)
      if (request.method === 'GET' && url.pathname === '/api/payments/epay/notify') {
        // Exact raw query forwarding preserves signature/duplicate validation.
        const upstream = httpRequest(`${configuration.backendUrl}${request.url}`, { method: 'GET', timeout: 10000 }, (result) => {
          response.writeHead(result.statusCode ?? 502, { 'content-type': 'text/plain' })
          result.pipe(response)
        })
        upstream.on('timeout', () => upstream.destroy())
        upstream.on('error', () => { if (!response.headersSent) response.writeHead(502); response.end('unavailable') })
        upstream.end(); return
      }
      if (request.method === 'GET' && url.pathname === '/return') {
        response.writeHead(302, { location: `${configuration.frontendUrl}/project/wallet`, 'cache-control': 'no-store' }); response.end(); return
      }
      const params = parseFixtureQuery(url.search.slice(1))
      if (request.method === 'GET' && url.pathname === '/epay/submit.php') {
        if (!verified(params) || !['alipay', 'wxpay'].includes(params.type) || params.notify_url !== notifyURL || params.return_url !== returnURL || !/^[a-f\d-]{36}$/i.test(params.out_trade_no ?? '') || !/^\d+\.\d{2}$/.test(params.money ?? '')) throw new Error('Invalid fixture checkout')
        const previous = orders.get(params.out_trade_no)
        if (previous && ['pid', 'type', 'money', 'notify_url', 'return_url'].some((field) => previous[field] !== params[field])) throw new Error('Fixture order conflict')
        if (!previous) orders.set(params.out_trade_no, { ...params, paid: false, trade_no: `fixture-${params.out_trade_no}` })
        response.writeHead(200, { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store', 'content-security-policy': "default-src 'none'; form-action 'self'; base-uri 'none'" })
        response.end(`<html><head><title>Local reference cashier</title></head><body><h1>Local reference cashier</h1><p>Fixture only: no real money</p><p>CNY ${html(params.money)} · ${html(params.type)}</p><form method="POST" action="/fixture/pay?order=${encodeURIComponent(params.out_trade_no)}"><button>Pay fixture order</button></form><a href="/return">Return without paying</a></body></html>`); return
      }
      if (request.method === 'POST' && url.pathname === '/fixture/pay') {
        const order = orders.get(params.order)
        if (!order) throw new Error('Fixture order missing')
        order.paid = true
        // Reference query endtime is an Asia/Shanghai local timestamp.
        const parts = Object.fromEntries(new Intl.DateTimeFormat('en-GB', {
          timeZone: 'Asia/Shanghai', year: 'numeric', month: '2-digit', day: '2-digit',
          hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23',
        }).formatToParts(new Date()).map(({ type, value }) => [type, value]))
        order.endtime = `${parts.year}-${parts.month}-${parts.day} ${parts.hour}:${parts.minute}:${parts.second}`
        const notice = { pid: order.pid, trade_no: order.trade_no, out_trade_no: order.out_trade_no, type: order.type, name: order.name, money: order.money, trade_status: 'TRADE_SUCCESS' }
        const query = new URLSearchParams({ ...notice, sign: signFixtureParams(notice), sign_type: 'MD5' })
        const accepted = await callback(`${notifyURL}?${query}`, tls.ca)
        response.writeHead(200, { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store' })
        response.end(`<html><body><h1>${accepted ? 'Fixture notification accepted' : 'Fixture notification pending'}</h1><a href="/return">Return to wallet</a></body></html>`); return
      }
      if (request.method === 'GET' && url.pathname === '/epay/api.php') {
        if (params.act !== 'order' || params.pid !== FIXTURE_MERCHANT || params.key !== FIXTURE_KEY) throw new Error('Invalid fixture query credentials')
        const order = params.trade_no ? [...orders.values()].find((item) => item.trade_no === params.trade_no) : orders.get(params.out_trade_no)
        response.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' })
        response.end(JSON.stringify(order ? { code: 1, pid: order.pid, trade_no: order.trade_no, out_trade_no: order.out_trade_no, type: order.type, name: order.name, money: order.money, endtime: order.endtime, status: order.paid ? 1 : 0 } : { code: -1 })); return
      }
      response.writeHead(404); response.end('not found')
    } catch {
      if (!response.headersSent) response.writeHead(400, { 'content-type': 'text/plain' })
      response.end('invalid fixture request')
    }
  })
  server.requestTimeout = 15000
  server.headersTimeout = 15000
  server.keepAliveTimeout = 1000
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(configuration.paymentPort, '127.0.0.1', resolve) })
  return server
}
