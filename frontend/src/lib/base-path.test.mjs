import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import ts from 'typescript';

const source = readFileSync(join(import.meta.dirname, 'base-path.ts'), 'utf8');
const transpiled = ts.transpileModule(source, {
  compilerOptions: {
    module: ts.ModuleKind.ESNext,
    target: ts.ScriptTarget.ES2023,
  },
}).outputText;

async function loadWithBasePath(content) {
  globalThis.document = {
    querySelector: () => (content === null ? null : { content }),
  };
  const moduleUrl = `data:text/javascript;base64,${Buffer.from(transpiled).toString('base64')}#${encodeURIComponent(String(content))}`;
  return import(moduleUrl);
}

test('prefixes and strips same-origin paths from the injected runtime base path', async () => {
  const { APP_BASE_PATH, withBasePath, withoutBasePath } = await loadWithBasePath('/gateway/');

  assert.equal(APP_BASE_PATH, '/gateway');
  assert.equal(withBasePath('/admin/graphql'), '/gateway/admin/graphql');
  assert.equal(withBasePath('/gateway/admin/graphql'), '/gateway/admin/graphql');
  assert.equal(withoutBasePath('/gateway/sign-in'), '/sign-in');
  assert.equal(withoutBasePath('/gateway'), '/');
  assert.equal(withoutBasePath('/gatewayish/sign-in'), '/gatewayish/sign-in');
});

test('keeps root deployments and external URLs unchanged', async () => {
  const { APP_BASE_PATH, withBasePath } = await loadWithBasePath(null);

  assert.equal(APP_BASE_PATH, '');
  assert.equal(withBasePath('/admin/graphql'), '/admin/graphql');
  assert.equal(withBasePath('https://example.invalid/api'), 'https://example.invalid/api');
  delete globalThis.document;
});
