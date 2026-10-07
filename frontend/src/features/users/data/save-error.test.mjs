import { MutationObserver, QueryCache, QueryClient } from '@tanstack/react-query';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';

const source = ts.transpileModule(readFileSync(new URL('./save-error.ts', import.meta.url), 'utf8'), {
  compilerOptions: { module: ts.ModuleKind.ESNext },
}).outputText;
const { classifyUserSaveError } = await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);
test('domain email errors target the field while permission and unknown failures stay distinct', () => {
  assert.equal(classifyUserSaveError(new Error("email 'a@example.test' already exists")), 'duplicate-email');
  assert.equal(classifyUserSaveError({ extensions: { code: 'DUPLICATE_EMAIL' } }), 'duplicate-email');
  assert.equal(classifyUserSaveError(new Error('permission denied: missing write_users')), 'forbidden');
  assert.equal(classifyUserSaveError(new Error('authz: only a platform owner may grant global owner')), 'forbidden');
  assert.equal(classifyUserSaveError({ status: 403 }), 'forbidden');
  assert.equal(classifyUserSaveError(new Error('database connection failed')), 'other');
});

function loadModule(text, dependencies) {
  const module = { exports: {} };
  const compiled = ts.transpileModule(text, { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText;
  new Function('require', 'module', 'exports', compiled)((name) => dependencies[name], module, module.exports);
  return module.exports;
}

const main = ts.createSourceFile(
  'main.tsx',
  readFileSync(new URL('../../../main.tsx', import.meta.url), 'utf8'),
  ts.ScriptTarget.Latest,
  true
);
let clientInitializer;
function findClient(node) {
  if (ts.isVariableDeclaration(node) && node.name.getText(main) === 'queryClient') clientInitializer = node.initializer.getText(main);
  ts.forEachChild(node, findClient);
}
findClient(main);
assert.ok(clientInitializer);
const dialog = ts.createSourceFile(
  'users-action-dialog.tsx',
  readFileSync(new URL('../components/users-action-dialog.tsx', import.meta.url), 'utf8'),
  ts.ScriptTarget.Latest,
  true
);
let submitCatch;
function findSubmit(node) {
  if (ts.isVariableDeclaration(node) && node.name.getText(dialog) === 'onSubmit') {
    submitCatch = node.initializer.body.statements.find(ts.isTryStatement).catchClause.getText(dialog);
  }
  ts.forEachChild(node, findSubmit);
}
findSubmit(dialog);
assert.ok(submitCatch);

for (const hook of ['useCreateUser', 'useUpdateUser']) {
  for (const [kind, error] of [
    ['duplicate-email', new Error("email 'a@example.test' already exists")],
    ['forbidden', new Error('permission denied: missing write_users')],
    ['other', new Error('database connection failed')],
  ]) {
    test(`${hook}: application defaults and real form catch give one ${kind} feedback`, async () => {
      const feedback = [];
      const generic = [];
      const toast = { error: (message) => feedback.push({ toast: message }) };
      const t = (key) => key;
      const clientCode = ts.transpileModule(
        `const client = ${clientInitializer.replaceAll('import.meta.env', '{ DEV: false, PROD: true }')}`,
        {
          compilerOptions: { module: ts.ModuleKind.ESNext },
        }
      ).outputText;
      const client = new Function('QueryClient', 'QueryCache', 'handleServerError', 'toast', 'i18n', `${clientCode}; return client;`)(
        QueryClient,
        QueryCache,
        (value) => generic.push(value),
        toast,
        { t }
      );
      // Prove these are the application's active defaults, not an empty client.
      await assert.rejects(
        new MutationObserver(client, {
          mutationFn: async () => {
            throw error;
          },
        }).mutate({})
      );
      assert.deepEqual(generic, [error]);
      generic.length = 0;
      const hooks = loadModule(readFileSync(new URL('./users.ts', import.meta.url), 'utf8'), {
        '@tanstack/react-query': {
          useQueryClient: () => client,
          useMutation: (options) => {
            const observer = new MutationObserver(client, options);
            return { mutateAsync: (variables) => observer.mutate(variables) };
          },
        },
        '@/gql/graphql': {
          graphqlRequest: async () => {
            throw error;
          },
        },
        '@/gql/users': {},
        'react-i18next': { useTranslation: () => ({ t }) },
        sonner: { toast },
        '@/hooks/use-error-handler': {},
        './schema': {},
      });
      const mutation = hooks[hook]();
      const form = { setError: (field, value, options) => feedback.push({ field, value, options }) };
      const catchCode = ts.transpileModule(`return async function submit() { try { await mutation.mutateAsync({}); } ${submitCatch} }`, {
        compilerOptions: { module: ts.ModuleKind.ESNext },
      }).outputText;
      const submit = new Function('mutation', 'classifyUserSaveError', 'form', 'toast', 't', catchCode)(
        mutation,
        classifyUserSaveError,
        form,
        toast,
        t
      );
      await submit();
      assert.deepEqual(generic, []);
      assert.equal(feedback.length, 1);
      if (kind === 'duplicate-email') {
        assert.deepEqual(feedback[0], {
          field: 'email',
          value: { type: 'server', message: 'users.errors.duplicateEmail' },
          options: { shouldFocus: true },
        });
      } else {
        assert.equal(feedback[0].toast, kind === 'forbidden' ? 'users.errors.saveForbidden' : 'common.errors.userSaveFailed');
      }
      client.clear();
    });
  }
}
