import assert from 'node:assert/strict'
import test from 'node:test'
import { parseFixtureQuery, signFixtureParams } from './payment-fixture.mjs'

test('pinned reference server derived vector retains zero/raw spaces and filters empty/sign fields', () => {
  const values = { a: '0', b: ' 空+%&=', c: '中文', empty: '', blank: ' ', sign: 'ignored', sign_type: 'MD5', array: ['ignored'], nil: null }
  assert.equal(signFixtureParams(values, 'test-key'), '1fe0ed95a23f863a44c705b0b5b412de')
  assert.equal(signFixtureParams({ z: '2', a: '1' }, 'key'), signFixtureParams({ a: '1', z: '2' }, 'key'))
})
test('strict fixture parsing decodes once and rejects duplicate/array/bad percent/UTF8 parameters', () => {
  const parsed = parseFixtureQuery('name=%E4%B8%AD%E6%96%87%2B%25%26%3D&zero=0&space=+')
  assert.equal(parsed.name, '中文+%&='); assert.equal(parsed.zero, '0'); assert.equal(parsed.space, ' ')
  for (const raw of ['pid=1&pid=2', 'pid=1&%70id=2', 'pid[]=1', 'name=%Q0', 'name=%FF']) assert.throws(() => parseFixtureQuery(raw), undefined, raw)
})
