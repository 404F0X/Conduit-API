import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const sourceRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');

function readSource(...segments) {
  return readFileSync(resolve(sourceRoot, ...segments), 'utf8');
}

test('the frontend does not discover or announce upstream versions', () => {
  const layout = readSource('authenticated-layout.tsx');
  const about = readSource('features', 'system', 'components', 'about-settings.tsx');
  const systemData = readSource('features', 'system', 'data', 'system.ts');

  for (const source of [layout, about, systemData]) {
    assert.doesNotMatch(source, /checkForUpdate|CheckForUpdate|useVersionCheck|useCheckForUpdate/);
  }
});
