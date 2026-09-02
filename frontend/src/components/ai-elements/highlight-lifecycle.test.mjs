import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import ts from 'typescript';

const source = readFileSync(join(import.meta.dirname, 'highlight-lifecycle.ts'), 'utf8');
const transpiled = ts.transpileModule(source, {
  compilerOptions: {
    module: ts.ModuleKind.ESNext,
    target: ts.ScriptTarget.ES2023,
  },
}).outputText;
const moduleUrl = `data:text/javascript;base64,${Buffer.from(transpiled).toString('base64')}`;
const { createHighlightState, settleHighlightWhileActive } = await import(moduleUrl);

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, reject, resolve };
}

async function flushPromiseCallbacks() {
  await Promise.resolve();
}

test('a superseded highlight cannot overwrite the latest result when promises settle out of order', async () => {
  const first = deferred();
  const second = deferred();
  const commits = [];

  const cancelFirst = settleHighlightWhileActive(first.promise, (state) => commits.push(state));
  cancelFirst();
  settleHighlightWhileActive(second.promise, (state) => commits.push(state));

  second.resolve(['second-light', 'second-dark']);
  await flushPromiseCallbacks();
  first.resolve(['first-light', 'first-dark']);
  await flushPromiseCallbacks();

  assert.deepEqual(commits, [{ light: 'second-light', dark: 'second-dark', isLoading: false }]);
});

test('a highlight that settles after unmount cannot commit state', async () => {
  const pending = deferred();
  const commits = [];

  const cancel = settleHighlightWhileActive(pending.promise, (state) => commits.push(state));
  cancel();
  pending.resolve(['late-light', 'late-dark']);
  await flushPromiseCallbacks();

  assert.deepEqual(commits, []);
});

test('switching from pre-rendered HTML to asynchronous highlighting restores loading state', () => {
  assert.deepEqual(createHighlightState({ light: 'ready-light', dark: 'ready-dark' }), {
    light: 'ready-light',
    dark: 'ready-dark',
    isLoading: false,
  });
  assert.deepEqual(createHighlightState(), {
    light: '',
    dark: '',
    isLoading: true,
  });
});

test('an active rejected highlight exits loading without leaking rejection details', async () => {
  const pending = deferred();
  const commits = [];

  settleHighlightWhileActive(pending.promise, (state) => commits.push(state));
  pending.reject(new Error('sensitive source text'));
  await flushPromiseCallbacks();

  assert.deepEqual(commits, [{ light: '', dark: '', isLoading: false }]);
});
