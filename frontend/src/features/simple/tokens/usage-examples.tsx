import { useTranslation } from 'react-i18next';
import { CopyButton } from '@/components/ui/copy-button';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';

/** 接入示例：地址取当前站点，令牌和模型直接填好，复制即可用。 */
export function UsageExamples({ apiKey, model }: { apiKey: string; model?: string }) {
  const { t } = useTranslation();
  const origin = typeof window !== 'undefined' ? window.location.origin : '';
  const sampleModel = model || 'gpt-4o-mini';

  const examples: Array<{ id: string; label: string; code: string }> = [
    {
      id: 'curl',
      label: 'curl',
      code: `curl ${origin}/v1/chat/completions \\
  -H "Authorization: Bearer ${apiKey}" \\
  -H "Content-Type: application/json" \\
  -d '{"model": "${sampleModel}", "messages": [{"role": "user", "content": "你好"}]}'`,
    },
    {
      id: 'openai',
      label: 'OpenAI SDK',
      code: `from openai import OpenAI

client = OpenAI(base_url="${origin}/v1", api_key="${apiKey}")
resp = client.chat.completions.create(
    model="${sampleModel}",
    messages=[{"role": "user", "content": "你好"}],
)
print(resp.choices[0].message.content)`,
    },
    {
      id: 'anthropic',
      label: 'Anthropic SDK',
      code: `from anthropic import Anthropic

client = Anthropic(base_url="${origin}/anthropic", api_key="${apiKey}")
msg = client.messages.create(
    model="${sampleModel}",
    max_tokens=1024,
    messages=[{"role": "user", "content": "你好"}],
)
print(msg.content[0].text)`,
    },
    {
      id: 'claude-code',
      label: 'Claude Code',
      code: `export ANTHROPIC_BASE_URL="${origin}/anthropic"
export ANTHROPIC_AUTH_TOKEN="${apiKey}"
claude`,
    },
    {
      id: 'codex',
      label: 'Codex',
      code: `# ~/.codex/config.toml
model = "${sampleModel}"
model_provider = "conduit"

[model_providers.conduit]
name = "Conduit"
base_url = "${origin}/v1"
env_key = "CONDUIT_API_KEY"

# 然后在终端里：
# export CONDUIT_API_KEY="${apiKey}"`,
    },
  ];

  return (
    <Tabs defaultValue='curl' className='w-full'>
      <TabsList className='flex h-auto flex-wrap justify-start'>
        {examples.map((example) => (
          <TabsTrigger key={example.id} value={example.id} className='text-xs'>
            {example.label}
          </TabsTrigger>
        ))}
      </TabsList>
      {examples.map((example) => (
        <TabsContent key={example.id} value={example.id}>
          <div className='bg-muted relative rounded-md'>
            <div className='absolute top-2 right-2'>
              <CopyButton content={example.code} copyMessage={t('simple.tokens.copied')} />
            </div>
            <pre className='overflow-x-auto p-3 pr-10 font-mono text-xs leading-relaxed whitespace-pre'>{example.code}</pre>
          </div>
        </TabsContent>
      ))}
    </Tabs>
  );
}
