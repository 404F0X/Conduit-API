import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import ts from 'typescript';

const sourcePath = join(import.meta.dirname, 'graphql.ts');
const source = readFileSync(sourcePath, 'utf8');
const transpiled = ts
  .transpileModule(source, {
    compilerOptions: {
      module: ts.ModuleKind.ESNext,
      target: ts.ScriptTarget.ES2023,
    },
  })
  .outputText.replaceAll("import { toast } from 'sonner';", 'const toast = { error() {} };')
  .replaceAll(
    "import { getTokenFromStorage, removeTokenFromStorage } from '@/stores/authStore';",
    'const getTokenFromStorage = () => ""; const removeTokenFromStorage = () => {};'
  )
  .replaceAll(
    "import { getProjectIdFromStorage } from '@/stores/projectStore';",
    'const getProjectIdFromStorage = () => globalThis.__projectId ?? null;'
  )
  .replaceAll("import { withBasePath } from '@/lib/base-path';", 'const withBasePath = (path) => path;')
  .replaceAll("import i18n from '@/lib/i18n';", 'const i18n = { t: (key) => key };');

const moduleUrl = `data:text/javascript;base64,${Buffer.from(transpiled).toString('base64')}`;
const { GraphQLRequestError, graphqlRequest, isUnauthorizedGraphQLError } = await import(moduleUrl);

test('does not classify upstream unauthorized provider failures as login expiration', () => {
  const error = {
    message: 'model fetch returned error: failed to fetch models: GET - https://provider.example/v1/models with status 401 Unauthorized',
    path: ['syncChannelModels'],
  };

  assert.equal(isUnauthorizedGraphQLError(error), false);
});

test('classifies explicit GraphQL authentication codes as login expiration', () => {
  assert.equal(isUnauthorizedGraphQLError({ extensions: { code: 'UNAUTHENTICATED' } }), true);
});

test('sends the selected project by default and allows an explicit override', async () => {
  const originalFetch = globalThis.fetch;
  const requests = [];
  globalThis.__projectId = 'gid://conduit/Project/7';
  globalThis.fetch = async (_url, init) => {
    requests.push(init);
    return new Response(JSON.stringify({ data: { ok: true } }), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    });
  };

  try {
    await graphqlRequest('query DefaultProject { ok }');
    await graphqlRequest('query OverrideProject { ok }', undefined, {
      'X-Project-ID': 'gid://conduit/Project/9',
    });
  } finally {
    globalThis.fetch = originalFetch;
    delete globalThis.__projectId;
  }

  assert.equal(requests[0].headers['X-Project-ID'], 'gid://conduit/Project/7');
  assert.equal(requests[1].headers['X-Project-ID'], 'gid://conduit/Project/9');
});

test('turns a successful HTTP response with GraphQL errors into a catchable non-auth error', async () => {
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async () =>
    new Response(
      JSON.stringify({
        data: null,
        errors: [{ message: 'credit redemption code is invalid or unavailable' }],
      }),
      {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }
    );

  try {
    await assert.rejects(graphqlRequest('mutation RedeemCreditCode { redeemCreditCode(code: "invalid") { id } }'), (error) => {
      assert.equal(error instanceof GraphQLRequestError, true);
      assert.equal(error.isAuthError, false);
      assert.equal(error.message, 'credit redemption code is invalid or unavailable');
      return true;
    });
  } finally {
    globalThis.fetch = originalFetch;
  }
});
