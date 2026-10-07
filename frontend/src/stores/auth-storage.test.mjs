import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';

const source = ts.transpileModule(readFileSync(new URL('./auth-storage.ts', import.meta.url), 'utf8'), {
  compilerOptions: { module: ts.ModuleKind.ESNext },
}).outputText;
const auth = await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);
const memoryStorage = () => {
  const values = new Map();
  return {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
    removeItem: (key) => values.delete(key),
  };
};

test('temporary login survives reload but not a new browser session; mode changes remove previous credentials', () => {
  globalThis.localStorage = memoryStorage();
  globalThis.sessionStorage = memoryStorage();
  auth.setTokenToStorage('persistent', true);
  auth.setUserToStorage({ id: 'old' });
  auth.setTokenToStorage('temporary', false);
  auth.setUserToStorage({ id: 'new' });
  assert.equal(auth.getTokenFromStorage(), 'temporary');
  assert.deepEqual(auth.getUserFromStorage(), { id: 'new' });
  assert.equal(localStorage.getItem(auth.ACCESS_TOKEN), null);
  globalThis.sessionStorage = memoryStorage();
  assert.equal(auth.getTokenFromStorage(), '');
  assert.equal(auth.getUserFromStorage(), null);
  auth.setTokenToStorage('remembered', true);
  auth.setUserToStorage({ id: 'saved' });
  globalThis.sessionStorage = memoryStorage();
  assert.equal(auth.getTokenFromStorage(), 'remembered');
  assert.deepEqual(auth.getUserFromStorage(), { id: 'saved' });
});

test('logout and expiration remove token and cached identity from both stores', () => {
  for (const persistent of [true, false]) {
    auth.setTokenToStorage('token', persistent);
    auth.setUserToStorage({ id: 'user' });
    auth.removeTokenFromStorage();
    assert.equal(auth.getTokenFromStorage(), '');
    assert.equal(auth.getUserFromStorage(), null);
    assert.equal(localStorage.getItem('conduit_user_info'), null);
    assert.equal(sessionStorage.getItem('conduit_user_info'), null);
  }
});
