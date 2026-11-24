export type ChatMessage = { role: 'user' | 'assistant' | 'system'; content: string };

// Deliberately never surface upstream bodies: they can echo credentials or prompts.
export class ChatRequestError extends Error {
  constructor(
    public readonly kind: 'http' | 'protocol' | 'disconnected' | 'network',
    public readonly status?: number
  ) {
    super(kind);
  }
}

export class PlaygroundSession {
  private key = '';
  private generation = 0;
  private controllers = new Set<AbortController>();

  setKey(key: string) {
    this.reset();
    this.key = key.trim();
  }

  reset() {
    this.generation++;
    for (const controller of this.controllers) controller.abort();
    this.controllers.clear();
    this.key = '';
  }

  start() {
    if (!this.key || /\s/.test(this.key)) throw new ChatRequestError('protocol');
    const generation = this.generation;
    const controller = new AbortController();
    this.controllers.add(controller);
    return {
      key: this.key,
      signal: controller.signal,
      abort: () => controller.abort(),
      current: () => generation === this.generation && !controller.signal.aborted,
      release: () => this.controllers.delete(controller),
    };
  }
}

async function request(url: string, key: string, signal: AbortSignal, body?: unknown) {
  let response: Response;
  try {
    response = await fetch(url, {
      method: body ? 'POST' : 'GET',
      credentials: 'omit',
      headers: { Authorization: `Bearer ${key}`, ...(body ? { 'Content-Type': 'application/json' } : {}) },
      signal,
      ...(body ? { body: JSON.stringify(body) } : {}),
    });
  } catch {
    throw new ChatRequestError('network');
  }
  if (!response.ok) {
    await response.body?.cancel();
    throw new ChatRequestError('http', response.status);
  }
  return response;
}

export async function fetchModels(url: string, key: string, signal: AbortSignal): Promise<string[]> {
  const response = await request(url, key, signal);
  try {
    const body = await response.json();
    if (!Array.isArray(body?.data)) throw new Error();
    return [
      ...new Set<string>(body.data.flatMap((item: { id?: unknown }) => (typeof item?.id === 'string' && item.id.trim() ? [item.id] : []))),
    ];
  } catch {
    throw new ChatRequestError('protocol');
  }
}

export async function readOpenAIStream(body: ReadableStream<Uint8Array>, onDelta: (content: string) => void, signal: AbortSignal) {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = '';
  let data: string[] = [];
  let done = false;
  const dispatch = () => {
    if (!data.length) return;
    const payload = data.join('\n');
    data = [];
    if (payload.trim() === '[DONE]') {
      done = true;
      return;
    }
    try {
      const event = JSON.parse(payload);
      if (event.error) throw new Error();
      const delta = event.choices?.[0]?.delta?.content;
      if (typeof delta === 'string') onDelta(delta);
    } catch {
      throw new ChatRequestError('protocol');
    }
  };
  const consume = (eof = false) => {
    // A trailing CR may be half of a CRLF split across network chunks.
    while (buffer.length) {
      const end = buffer.search(/[\r\n]/);
      if (end < 0 || (!eof && end === buffer.length - 1 && buffer[end] === '\r')) break;
      const line = buffer.slice(0, end);
      buffer = buffer.slice(end + (buffer[end] === '\r' && buffer[end + 1] === '\n' ? 2 : 1));
      if (line === '') dispatch();
      else if (line.startsWith('data:')) data.push(line.slice(5).replace(/^ /, ''));
      if (done) break;
    }
  };
  const abort = () => {
    void reader.cancel().catch(() => {});
  };
  signal.addEventListener('abort', abort, { once: true });
  try {
    if (signal.aborted) return;
    while (!done && !signal.aborted) {
      const chunk = await reader.read();
      buffer += chunk.done ? decoder.decode() : decoder.decode(chunk.value, { stream: true });
      consume(chunk.done);
      if (chunk.done) break;
    }
    if (!done && !signal.aborted) throw new ChatRequestError('disconnected');
  } catch (error) {
    if (error instanceof ChatRequestError) throw error;
    throw new ChatRequestError('disconnected');
  } finally {
    signal.removeEventListener('abort', abort);
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

export function retryMessages(messages: ChatMessage[]) {
  const lastUser = messages.findLastIndex((message) => message.role === 'user');
  return lastUser < 0 ? [] : messages.slice(0, lastUser + 1);
}

export function validSettings(model: string, temperature: number, maxTokens: number) {
  return (
    !!model.trim() &&
    Number.isFinite(temperature) &&
    temperature >= 0 &&
    temperature <= 2 &&
    Number.isSafeInteger(maxTokens) &&
    maxTokens > 0
  );
}

export async function streamChat(
  url: string,
  key: string,
  signal: AbortSignal,
  body: { model: string; messages: ChatMessage[]; temperature: number; max_tokens: number },
  onDelta: (content: string) => void
) {
  if (!validSettings(body.model, body.temperature, body.max_tokens)) throw new ChatRequestError('protocol');
  const response = await request(url, key, signal, { ...body, stream: true });
  if (!response.headers.get('content-type')?.includes('text/event-stream') || !response.body) {
    await response.body?.cancel();
    throw new ChatRequestError('protocol');
  }
  await readOpenAIStream(response.body, onDelta, signal);
}
