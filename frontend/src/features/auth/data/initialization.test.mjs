import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';

const source = readFileSync(join(import.meta.dirname, 'initialization.ts'), 'utf8');

test('successful initialization replaces the cached pre-initialization status before navigation', () => {
  const successHandler = source.slice(source.indexOf('onSuccess:'), source.indexOf('onError:'));

  assert.match(source, /queryKey:\s*systemStatusQueryKey/);
  assert.match(successHandler, /setQueryData<SystemStatus>\(systemStatusQueryKey, \{ isInitialized: true \}\)/);
  assert.ok(successHandler.indexOf('setQueryData') < successHandler.indexOf("router.navigate({ to: '/sign-in' })"));
});
