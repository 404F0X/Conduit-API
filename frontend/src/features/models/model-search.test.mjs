import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import ts from 'typescript';

const source = readFileSync(join(import.meta.dirname, 'model-search.ts'), 'utf8');
const output = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 },
}).outputText;
const module = { exports: {} };
new Function('require', 'module', 'exports', output)(() => ({}), module, module.exports);

const { validateModelsSearch } = module.exports;

test('models search validation opens public-model detail and discards channel params', () => {
  assert.deepEqual(validateModelsSearch({ view: 'models', model: 'gpt-4o', channel: 'channel-a', upstreamModel: 'vendor-model' }), {
    view: 'models',
    model: 'gpt-4o',
  });
});

test('models search validation opens channel detail with optional upstream model', () => {
  assert.deepEqual(
    validateModelsSearch({ view: 'channels', channel: 'channel-a', upstreamModel: 'vendor-model', deployment: 'deployment-a' }),
    {
      view: 'channels',
      channel: 'channel-a',
      upstreamModel: 'vendor-model',
      deployment: 'deployment-a',
    }
  );
});

test('upstream discovery deep link preserves the exact channel and upstream model detail', () => {
  const search = Object.fromEntries(
    new URLSearchParams('view=channels&channel=gid%3A%2F%2Fconduit%2FChannel%2F47&upstreamModel=claude-sonnet-5')
  );

  assert.deepEqual(validateModelsSearch(search), {
    view: 'channels',
    channel: 'gid://conduit/Channel/47',
    upstreamModel: 'claude-sonnet-5',
  });
});

test('deployment GID can address an upstream detail without relying on the model text', () => {
  assert.deepEqual(
    validateModelsSearch({
      view: 'channels',
      channel: 'gid://conduit/Channel/47',
      deployment: 'gid://conduit/UpstreamModelDeployment/512',
    }),
    {
      view: 'channels',
      channel: 'gid://conduit/Channel/47',
      deployment: 'gid://conduit/UpstreamModelDeployment/512',
    }
  );
});

test('deployment selection is discarded outside the channels view or without a channel', () => {
  assert.deepEqual(validateModelsSearch({ view: 'models', model: 'gpt-4o', deployment: 'deployment-a' }), {
    view: 'models',
    model: 'gpt-4o',
  });
  assert.deepEqual(validateModelsSearch({ view: 'channels', deployment: 'deployment-a' }), { view: 'channels' });
});

test('models search validation infers view and rejects invalid empty values', () => {
  assert.deepEqual(validateModelsSearch({ model: 'gpt-4o', view: 'bad' }), { view: 'models', model: 'gpt-4o' });
  assert.deepEqual(validateModelsSearch({ channel: '', upstreamModel: 'ignored' }), {});
});
