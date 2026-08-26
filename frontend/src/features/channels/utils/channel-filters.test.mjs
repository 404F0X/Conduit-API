import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import ts from 'typescript';

const source = readFileSync(join(import.meta.dirname, 'channel-filters.ts'), 'utf8');
const output = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 },
}).outputText;
const module = { exports: {} };
new Function('require', 'module', 'exports', output)(() => ({}), module, module.exports);
const { buildChannelWhereClause } = module.exports;

const base = {
  nameFilter: '',
  typeFilter: [],
  tabFilteredTypes: [],
  statusFilter: [],
  showErrorOnly: false,
};

test('defaults to enabled and disabled channels', () => {
  assert.deepEqual(buildChannelWhereClause(base), {
    statusIn: ['enabled', 'disabled'],
  });
});

test('combines ordinary type, status, name and error filters', () => {
  assert.deepEqual(
    buildChannelWhereClause({
      ...base,
      nameFilter: 'gateway',
      typeFilter: ['openai'],
      tabFilteredTypes: ['openai', 'openai_responses'],
      statusFilter: ['disabled'],
      showErrorOnly: true,
    }),
    {
      nameContainsFold: 'gateway',
      typeIn: ['openai', 'openai_responses'],
      statusIn: ['disabled'],
      errorMessageNotNil: true,
    }
  );
});
