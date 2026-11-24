import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';

function load(file, require = () => ({})) {
  const output = ts.transpileModule(readFileSync(new URL(file, import.meta.url), 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023, jsx: ts.JsxEmit.ReactJSX },
  }).outputText;
  const module = { exports: {} };
  new Function('require', 'module', 'exports', output)(require, module, module.exports);
  return module.exports;
}
const client = load('./chat-client.ts');
const key = 'fixture-playground-key';
const encoder = new TextEncoder();
function stream(text, split = 1) {
  const bytes = encoder.encode(text);
  return new ReadableStream({
    start(controller) {
      for (let i = 0; i < bytes.length; i += split) controller.enqueue(bytes.slice(i, i + split));
      controller.close();
    },
  });
}

test('SSE handles split UTF8, CRLF, multiline data, comments, usage and DONE then cancels', async () => {
  let canceled = false;
  const text =
    ': comment\r\ndata: {"choices":\r\ndata: [{"delta":{"content":"你好🌍"}}]}\r\n\r\ndata: {"choices":[],"usage":{"total_tokens":1}}\n\ndata: [DONE]\n\n';
  const source = stream(text);
  let content = '';
  await client.readOpenAIStream(
    source,
    (delta) => {
      content += delta;
    },
    new AbortController().signal
  );
  assert.equal(content, '你好🌍');
  const open = new ReadableStream({
    start(controller) {
      controller.enqueue(encoder.encode('data: [DONE]\n\n'));
    },
    cancel() {
      canceled = true;
    },
  });
  await client.readOpenAIStream(open, () => {}, new AbortController().signal);
  assert.equal(canceled, true);
  assert.equal(open.locked, false);
});

test('malformed JSON, stream errors and missing DONE fail without exposing raw payloads', async () => {
  for (const payload of ['data: broken\n\n', `data: {"error":{"message":"${key}"}}\n\n`, 'data: {"choices":[]}\n\n']) {
    await assert.rejects(
      client.readOpenAIStream(stream(payload), () => {}, new AbortController().signal),
      (error) => {
        assert.ok(error instanceof client.ChatRequestError);
        assert.ok(!error.message.includes(key));
        return true;
      }
    );
  }
});

test('stop cancels a blocked read and releases the reader', async () => {
  let canceled = false;
  const body = new ReadableStream({
    cancel() {
      canceled = true;
    },
  });
  const controller = new AbortController();
  const reading = client.readOpenAIStream(body, () => assert.fail('unexpected delta'), controller.signal);
  controller.abort();
  await reading;
  assert.equal(canceled, true);
  assert.equal(body.locked, false);
});

test('requests use only key auth, standard body and omit cookies; HTTP and JSON failures stay redacted', async (t) => {
  const original = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = original;
  });
  const calls = [];
  globalThis.fetch = async (url, init) => {
    calls.push({ url, init });
    return url.endsWith('/models')
      ? Response.json({ data: [{ id: 'fixture-model' }, { id: 'fixture-model' }, {}] })
      : new Response(stream('data: {"choices":[{"delta":{"content":"ok"}}]}\n\ndata: [DONE]\n\n'), {
          headers: { 'Content-Type': 'text/event-stream' },
        });
  };
  const signal = new AbortController().signal;
  assert.deepEqual(await client.fetchModels('/prefix/v1/models', key, signal), ['fixture-model']);
  const body = {
    model: 'fixture-model',
    temperature: 0.6,
    max_tokens: 4096,
    messages: [
      { role: 'system', content: 'fixture system' },
      { role: 'user', content: 'fixture input' },
    ],
  };
  let answer = '';
  await client.streamChat('/prefix/v1/chat/completions', key, signal, body, (delta) => {
    answer += delta;
  });
  assert.equal(answer, 'ok');
  assert.equal(calls.length, 2);
  assert.equal(calls[0].init.credentials, 'omit');
  assert.deepEqual(calls[0].init.headers, { Authorization: `Bearer ${key}` });
  assert.deepEqual(JSON.parse(calls[1].init.body), { ...body, stream: true });
  assert.deepEqual(Object.keys(calls[1].init.headers).sort(), ['Authorization', 'Content-Type']);
  globalThis.fetch = async () => Response.json({ error: { message: key } }, { status: 403 });
  await assert.rejects(client.fetchModels('/models', key, signal), (error) => error.status === 403 && !error.message.includes(key));
  globalThis.fetch = async () => Response.json({ error: { message: key } });
  await assert.rejects(client.fetchModels('/models', key, signal), /protocol/);
  await assert.rejects(
    client.streamChat('/chat', key, signal, body, () => {}),
    /protocol/
  );
});

test('session reset/key replacement aborts all work and invalidates delayed results; retry preserves the last user turn', () => {
  const session = new client.PlaygroundSession();
  session.setKey(key);
  const models = session.start();
  const chat = session.start();
  session.setKey('fixture-next-key');
  assert.equal(models.signal.aborted, true);
  assert.equal(chat.current(), false);
  const next = session.start();
  assert.equal(next.key, 'fixture-next-key');
  session.reset();
  assert.equal(next.signal.aborted, true);
  assert.throws(() => session.start(), /protocol/);
  const history = [
    { role: 'user', content: 'first' },
    { role: 'assistant', content: 'answer' },
    { role: 'user', content: 'second' },
    { role: 'assistant', content: 'partial' },
  ];
  assert.deepEqual(client.retryMessages(history), history.slice(0, 3));
  assert.equal(client.validSettings('model', NaN, 1), false);
  assert.equal(client.validSettings('model', 1, 1.5), false);
  assert.equal(client.validSettings('model', 1, 0), false);
});

// A hook harness exercises the real page's handlers and effects without a DOM or provider.
function pageHarness() {
  const slots = [];
  let cursor = 0;
  let effects = [];
  let project = 'fixture-project';
  let token = 'fixture-admin-token';
  const hooks = {
    useState(initial) {
      const i = cursor++;
      if (!(i in slots)) slots[i] = typeof initial === 'function' ? initial() : initial;
      return [
        slots[i],
        (value) => {
          slots[i] = typeof value === 'function' ? value(slots[i]) : value;
        },
      ];
    },
    useRef(initial) {
      const [ref] = hooks.useState(() => ({ current: initial }));
      return ref;
    },
    useCallback(fn) {
      return fn;
    },
    useEffect(fn, deps) {
      const i = cursor++;
      const previous = slots[i];
      if (!previous || deps.some((dep, j) => !Object.is(dep, previous.deps[j]))) {
        effects.push(() => {
          previous?.cleanup?.();
          slots[i] = { deps, cleanup: fn() };
        });
      }
    },
  };
  // useCallback identity is stable in React; our reset handler only depends on session.
  hooks.useCallback = (fn, deps) => {
    const i = cursor++;
    if (!slots[i] || deps.some((dep, j) => dep !== slots[i].deps[j])) slots[i] = { deps, fn };
    return slots[i].fn;
  };
  const jsx = (type, props) => ({ type, props });
  const mocks = new Proxy({}, { get: (_, prop) => prop });
  const { default: Page } = load('./index.tsx', (name) => {
    if (name === 'react') return hooks;
    if (name === 'react/jsx-runtime') return { jsx, jsxs: jsx };
    if (name === './chat-client') return client;
    if (name.includes('authStore')) return { useAuthStore: (selector) => selector({ auth: { accessToken: token } }) };
    if (name.includes('projectStore')) return { useSelectedProjectId: () => project };
    if (name.includes('base-path')) return { withBasePath: (path) => '/prefix' + path };
    if (name === 'react-i18next') return { useTranslation: () => ({ t: (key) => key }) };
    return mocks;
  });
  let tree;
  const render = () => {
    cursor = 0;
    tree = Page();
    const pending = effects;
    effects = [];
    pending.forEach((fn) => fn());
    cursor = 0;
    tree = Page();
    return tree;
  };
  const nodes = () => {
    const result = [];
    const walk = (node) => {
      if (!node || typeof node !== 'object') return;
      if (Array.isArray(node)) {
        node.forEach(walk);
        return;
      }
      result.push(node);
      walk(node.props?.children);
    };
    walk(tree);
    return result;
  };
  return {
    render,
    find: (predicate) => nodes().find(predicate),
    switchProject() {
      project = 'fixture-other-project';
      render();
    },
    logout() {
      token = '';
      render();
    },
    unmount() {
      slots.forEach((slot) => slot?.cleanup?.());
    },
  };
}

test('page loads key models, sends once, stops, retries once and clears/aborts on project, logout and unmount', async (t) => {
  const original = globalThis.fetch;
  t.after(() => {
    globalThis.fetch = original;
  });
  const requests = [];
  globalThis.fetch = async (url, init) => {
    requests.push({ url, init });
    return url.endsWith('/models')
      ? Response.json({ data: [{ id: 'fixture-model' }] })
      : new Response(
          new ReadableStream({
            start(controller) {
              controller.enqueue(encoder.encode('data: {"choices":[{"delta":{"content":"partial"}}]}\n\n'));
            },
          }),
          { headers: { 'Content-Type': 'text/event-stream' } }
        );
  };
  const page = pageHarness();
  const flush = async () => {
    await new Promise((resolve) => setImmediate(resolve));
    page.render();
  };
  const enterKey = () => {
    page.find((node) => node.props?.id === 'playground-api-key').props.onChange({ target: { value: key } });
    page.render();
  };
  const loadModels = async () => {
    page.find((node) => node.type === 'Button' && node.props.children === 'playground.settings.loadModels').props.onClick();
    await flush();
  };
  const event = { preventDefault() {} };
  page.render();
  enterKey();
  assert.equal(page.find((node) => node.props?.id === 'playground-api-key').props.type, 'password');
  await loadModels();
  assert.equal(page.find((node) => node.type === 'AutoCompleteSelect').props.selectedValue, 'fixture-model');
  page.find((node) => node.type === 'PromptInput').props.onSubmit({ text: 'fixture input' }, event);
  await flush();
  assert.equal(requests.filter((r) => r.url.endsWith('/completions')).length, 1);
  page.find((node) => node.type === 'PromptInputSubmit').props.onClick(event);
  page.render();
  assert.equal(requests.at(-1).init.signal.aborted, true);
  page.find((node) => node.type === 'Button' && node.props.children?.includes?.('playground.chat.retry')).props.onClick();
  await flush();
  assert.equal(requests.filter((r) => r.url.endsWith('/completions')).length, 2);
  assert.equal(JSON.parse(requests.at(-1).init.body).messages.filter((message) => message.role === 'user').length, 1);
  page.switchProject();
  page.render();
  assert.equal(requests.at(-1).init.signal.aborted, true);
  assert.equal(page.find((node) => node.props?.id === 'playground-api-key').props.value, '');
  enterKey();
  await loadModels();
  page.logout();
  page.render();
  assert.equal(page.find((node) => node.props?.id === 'playground-api-key').props.value, '');
  enterKey();
  await loadModels();
  page.find((node) => node.type === 'PromptInput').props.onSubmit({ text: 'fixture input' }, event);
  await flush();
  page.unmount();
  assert.equal(requests.at(-1).init.signal.aborted, true);
});
