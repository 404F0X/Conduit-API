import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import ts from 'typescript';

const source = readFileSync(join(import.meta.dirname, 'failure-classifier.ts'), 'utf8');
const output = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 },
}).outputText;
const module = { exports: {} };
new Function('require', 'module', 'exports', output)(() => ({}), module, module.exports);
const { classifyFailureText, classifyChannelFailure } = module.exports;

test('classifies common upstream failure families', () => {
  assert.equal(classifyFailureText('401 Unauthorized: invalid token'), 'auth');
  assert.equal(classifyFailureText('insufficient_quota'), 'quota');
  assert.equal(classifyFailureText('HTTP 429 too many requests'), 'rate_limit');
  assert.equal(classifyFailureText('model does not exist'), 'model');
  assert.equal(classifyFailureText('connection refused'), 'unreachable');
  assert.equal(classifyFailureText('502 Bad Gateway'), 'upstream');
  assert.equal(classifyFailureText('invalid JSON response'), 'protocol');
});

test('uses quota and disabled-key evidence when channel error is empty', () => {
  const result = classifyChannelFailure({
    errorMessage: null,
    providerQuotaStatus: { status: 'exhausted' },
    disabledAPIKeys: [{ key: 'sk-…', disabledAt: '', errorCode: 429, reason: 'rate limited' }],
  });
  assert.equal(result.kind, 'quota');
  assert.equal(result.evidence.length, 2);
});

test('does not turn an available quota snapshot into a failure', () => {
  assert.equal(classifyChannelFailure({ errorMessage: null, providerQuotaStatus: { status: 'available' }, disabledAPIKeys: [] }), null);
});
