import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import ts from 'typescript';

const source = readFileSync(join(import.meta.dirname, 'channel-management-adapter.ts'), 'utf8');
const output = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 },
}).outputText;
const module = { exports: {} };
new Function('require', 'module', 'exports', output)(() => ({}), module, module.exports);
const { isNewApiChannelTag } = module.exports;

test('recognizes user tags that duplicate the NEW API system label', () => {
  for (const tag of ['NEW API', 'new-api', 'new_api', ' New  Api ']) {
    assert.equal(isNewApiChannelTag(tag), true, tag);
  }
});

test('does not hide unrelated user tags', () => {
  for (const tag of ['new', 'api', 'new api provider', 'production']) {
    assert.equal(isNewApiChannelTag(tag), false, tag);
  }
});
