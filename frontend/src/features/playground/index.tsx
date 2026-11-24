import { useState, useEffect, useCallback, useRef } from 'react';
import { IconTrash, IconRefresh } from '@tabler/icons-react';
import { MessageSquare, RefreshCcw, Copy } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { useAuthStore } from '@/stores/authStore';
import { useSelectedProjectId } from '@/stores/projectStore';
import { withBasePath } from '@/lib/base-path';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { ScrollArea } from '@/components/ui/scroll-area';
import { Textarea } from '@/components/ui/textarea';
import { TooltipProvider } from '@/components/ui/tooltip';
import { Actions, Action } from '@/components/ai-elements/actions';
import { Conversation, ConversationContent, ConversationEmptyState, ConversationScrollButton } from '@/components/ai-elements/conversation';
import { Loader } from '@/components/ai-elements/loader';
import { Message, MessageContent } from '@/components/ai-elements/message';
import { PromptInput, PromptInputTextarea, PromptInputSubmit } from '@/components/ai-elements/prompt-input';
import { Response as UIResponse } from '@/components/ai-elements/response';
import { AutoCompleteSelect } from '@/components/auto-complete-select';
import {
  ChatRequestError,
  PlaygroundSession,
  fetchModels,
  retryMessages,
  streamChat,
  validSettings,
  type ChatMessage,
} from './chat-client';

export default function Playground() {
  const { t } = useTranslation();
  const selectedProjectId = useSelectedProjectId();
  const accessToken = useAuthStore((state) => state.auth.accessToken);
  const [session] = useState(() => new PlaygroundSession());
  const [apiKey, setApiKey] = useState('');
  const [models, setModels] = useState<string[]>([]);
  const [modelsLoading, setModelsLoading] = useState(false);
  const [model, setModel] = useState('');
  const [temperature, setTemperature] = useState(0.6);
  const [maxTokens, setMaxTokens] = useState(4096);
  const [systemPrompt, setSystemPrompt] = useState(t('playground.settings.defaultSystemPrompt'));
  const [input, setInput] = useState('');
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [status, setStatus] = useState<'ready' | 'submitted' | 'streaming'>('ready');
  const [error, setError] = useState('');
  const pending = useRef<ReturnType<PlaygroundSession['start']> | null>(null);
  const isLoading = status !== 'ready';
  const hasUserMessage = messages.some((message) => message.role === 'user');
  const modelOptions = models.map((id) => ({ value: id, label: id }));
  const canSend = !!apiKey.trim() && models.includes(model) && validSettings(model, temperature, maxTokens);

  const reset = useCallback(() => {
    session.reset();
    pending.current = null;
    setApiKey('');
    setModels([]);
    setModel('');
    setMessages([]);
    setInput('');
    setError('');
    setStatus('ready');
    setModelsLoading(false);
  }, [session]);

  useEffect(() => {
    reset();
    return () => session.reset();
  }, [selectedProjectId, accessToken, session, reset]);

  const showError = (cause: unknown) => {
    setError(
      cause instanceof ChatRequestError && cause.status
        ? t('playground.errors.http', { status: cause.status })
        : t('playground.errors.request')
    );
  };

  const loadModels = async () => {
    if (!apiKey.trim() || /\s/.test(apiKey.trim()) || modelsLoading || isLoading) return;
    session.setKey(apiKey);
    const operation = session.start();
    setModelsLoading(true);
    setError('');
    setModels([]);
    setModel('');
    try {
      const available = await fetchModels(withBasePath('/v1/models'), operation.key, operation.signal);
      if (operation.current()) {
        setModels(available);
        setModel(available[0] ?? '');
      }
    } catch (cause) {
      if (operation.current()) showError(cause);
    } finally {
      operation.release();
      if (operation.current()) setModelsLoading(false);
    }
  };

  const send = async (history: ChatMessage[]) => {
    if (!canSend || pending.current || !history.length) return;
    const operation = session.start();
    pending.current = operation;
    setError('');
    setStatus('submitted');
    const next: ChatMessage[] = [...history, { role: 'assistant', content: '' }];
    setMessages(next);
    let content = '';
    try {
      await streamChat(
        withBasePath('/v1/chat/completions'),
        operation.key,
        operation.signal,
        {
          model,
          temperature,
          max_tokens: maxTokens,
          messages: [...(systemPrompt.trim() ? [{ role: 'system' as const, content: systemPrompt }] : []), ...history],
        },
        (delta) => {
          if (!operation.current()) return;
          content += delta;
          setStatus('streaming');
          setMessages([...history, { role: 'assistant', content }]);
        }
      );
    } catch (cause) {
      if (operation.current()) showError(cause);
    } finally {
      operation.release();
      if (pending.current === operation) {
        pending.current = null;
        setStatus('ready');
      }
    }
  };
  const stop = () => {
    pending.current?.abort();
    pending.current = null;
    setStatus('ready');
  };
  const handleSubmit = (message: { text?: string }, e: React.FormEvent) => {
    e.preventDefault();
    if (!canSend || isLoading || !message.text?.trim()) return;
    void send([...messages, { role: 'user', content: message.text }]);
    setInput('');
  };
  const handleRetry = () => {
    void send(retryMessages(messages));
  };
  const handleClear = () => {
    setMessages([]);
    setError('');
  };

  return (
    <TooltipProvider>
      <div className='bg-background flex h-screen w-full flex-col md:flex-row'>
        {/* Settings Sidebar */}

        <div className='bg-card shadow-soft border-border m-4 flex max-h-[40vh] w-auto flex-col rounded-2xl border border-r md:max-h-none md:w-[340px] md:max-w-[400px] md:min-w-[280px]'>
          <div className='border-b p-4'>
            <h1 className='text-xl font-bold tracking-tight'>{t('playground.title')}</h1>
            <p className='text-muted-foreground mt-1 text-xs leading-relaxed'>{t('playground.description')}</p>
          </div>

          <ScrollArea className='min-h-0 flex-1 p-4'>
            <div className='space-y-6'>
              <div className='space-y-3'>
                <Label htmlFor='playground-api-key'>{t('playground.settings.apiKey')}</Label>
                <Input
                  id='playground-api-key'
                  type='password'
                  autoComplete='off'
                  value={apiKey}
                  onChange={(event) => {
                    reset();
                    setApiKey(event.target.value);
                    session.setKey(event.target.value);
                  }}
                />
                <p className='text-muted-foreground text-xs'>{t('playground.settings.keyNotice')}</p>
                <a className='text-primary text-xs underline' href={withBasePath('/project/api-keys')}>
                  {t('playground.settings.createKey')}
                </a>
                <Button
                  variant='outline'
                  className='w-full'
                  disabled={!apiKey.trim() || /\s/.test(apiKey.trim()) || modelsLoading || isLoading}
                  onClick={() => {
                    void loadModels();
                  }}
                >
                  {modelsLoading ? t('loading') : t('playground.settings.loadModels')}
                </Button>
              </div>

              <div className='space-y-3'>
                <Label htmlFor='model' className='text-xs font-semibold'>
                  {t('playground.settings.model')}
                </Label>
                <AutoCompleteSelect
                  selectedValue={model}
                  onSelectedValueChange={(v) => setModel(v)}
                  items={modelOptions}
                  isLoading={modelsLoading}
                  emptyMessage={t('playground.errors.noModelsAvailable')}
                  placeholder={modelsLoading ? t('loading') : t('playground.settings.selectModel')}
                />
                {modelsLoading && <p className='text-muted-foreground text-[10px]'>{t('loading')}...</p>}
              </div>

              <div className='space-y-3'>
                <Label htmlFor='temperature' className='text-xs font-semibold'>
                  {t('playground.settings.temperature')}: {temperature}
                </Label>
                <div className='px-1'>
                  <Input
                    id='temperature'
                    type='range'
                    min='0'
                    max='2'
                    step='0.1'
                    value={temperature}
                    onChange={(e) => setTemperature(parseFloat(e.target.value))}
                    className='bg-muted h-2 w-full cursor-pointer appearance-none rounded-lg'
                  />
                  <div className='text-muted-foreground mt-1 flex justify-between text-[10px]'>
                    <span>0</span>
                    <span>1</span>
                    <span>2</span>
                  </div>
                </div>
              </div>

              <div className='space-y-3'>
                <Label htmlFor='maxTokens' className='text-xs font-semibold'>
                  {t('playground.settings.maxTokens')}
                </Label>
                <Input
                  id='maxTokens'
                  type='number'
                  min='1'
                  max='2147483647'
                  value={maxTokens}
                  onChange={(e) => setMaxTokens(Number(e.target.value))}
                  className='h-9'
                />
              </div>

              <div className='space-y-3'>
                <Label htmlFor='systemPrompt' className='text-xs font-semibold'>
                  {t('playground.settings.systemPrompt')}
                </Label>
                <Textarea
                  id='systemPrompt'
                  placeholder={t('playground.settings.defaultSystemPrompt')}
                  value={systemPrompt}
                  onChange={(e) => setSystemPrompt(e.target.value)}
                  rows={4}
                  className='min-h-[80px] resize-none text-sm'
                />
              </div>
            </div>
          </ScrollArea>

          <div className='space-y-2 border-t p-4'>
            <p className='text-muted-foreground text-xs'>{t('playground.chat.retryNotice')}</p>
            <Button
              onClick={handleRetry}
              variant='outline'
              className='h-9 w-full text-xs'
              disabled={isLoading || !canSend || !hasUserMessage}
            >
              <IconRefresh className='mr-2 h-3 w-3' />
              {isLoading
                ? t('playground.chat.generating')
                : messages.length === 0
                  ? t('playground.chat.noMessages')
                  : messages.every((msg) => msg.role !== 'assistant')
                    ? t('playground.chat.noMessages')
                    : t('playground.chat.retry')}
            </Button>

            <Button onClick={handleClear} variant='outline' className='h-9 w-full text-xs' disabled={isLoading}>
              <IconTrash className='mr-2 h-3 w-3' />
              {t('playground.chat.clear')}
            </Button>
          </div>
        </div>

        {/* Chat Area */}
        <div className='flex flex-1 flex-col p-4'>
          <div className='shadow-soft border-border bg-card flex h-full flex-col rounded-2xl border p-6'>
            <Conversation className='max-h-[50vh] flex-1 md:max-h-none'>
              <ConversationContent>
                {messages.length === 0 ? (
                  <ConversationEmptyState
                    icon={<MessageSquare className='size-12' />}
                    title={t('playground.chat.startConversation')}
                    description={t('playground.chat.typeMessageBelow')}
                  />
                ) : (
                  (() => {
                    const lastAssistantIndex = (() => {
                      for (let i = messages.length - 1; i >= 0; i--) {
                        if (messages[i].role === 'assistant') return i;
                      }
                      return -1;
                    })();
                    return messages.map((message, messageIndex) => {
                      const isLastAssistant = message.role === 'assistant' && messageIndex === lastAssistantIndex;
                      const textContent = message.content;
                      return (
                        <Message from={message.role} key={messageIndex} fullWidth={!hasUserMessage}>
                          <MessageContent>
                            <UIResponse>{message.content}</UIResponse>
                            {isLastAssistant && textContent ? (
                              <Actions className='mt-2'>
                                <Action disabled={isLoading || !canSend} onClick={handleRetry} label={t('playground.chat.retry')}>
                                  <RefreshCcw className='size-3' />
                                </Action>
                                <Action onClick={() => navigator.clipboard.writeText(textContent)} label={t('copy')}>
                                  <Copy className='size-3' />
                                </Action>
                              </Actions>
                            ) : null}
                          </MessageContent>
                        </Message>
                      );
                    });
                  })()
                )}
                {status === 'submitted' && <Loader />}
              </ConversationContent>
              <ConversationScrollButton />
            </Conversation>

            {error && (
              <p role='alert' className='text-destructive text-sm'>
                {error}
              </p>
            )}
            <PromptInput onSubmit={handleSubmit} className='relative mt-4 w-full'>
              <PromptInputTextarea
                value={input}
                placeholder={t('playground.chat.typeMessage')}
                onChange={(e) => setInput(e.currentTarget.value)}
                className='pr-16'
              />
              <PromptInputSubmit
                status={status}
                disabled={status === 'ready' ? !input.trim() || !canSend : false}
                className='absolute right-3 bottom-3'
                onClick={(e) => {
                  // Stop the current request instead of submitting another one.
                  if (status !== 'ready') {
                    e.preventDefault();
                    stop();
                  }
                }}
              />
            </PromptInput>
          </div>
        </div>
      </div>
    </TooltipProvider>
  );
}
