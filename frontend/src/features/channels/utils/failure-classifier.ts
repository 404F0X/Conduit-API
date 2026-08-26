import type { Channel } from '../data/schema';

export type ChannelFailureKind = 'auth' | 'quota' | 'rate_limit' | 'model' | 'unreachable' | 'upstream' | 'protocol' | 'unknown';

export interface ChannelFailureEvidence {
  kind: ChannelFailureKind;
  evidence: string[];
}

const rules: Array<{ kind: ChannelFailureKind; pattern: RegExp }> = [
  { kind: 'auth', pattern: /\b(401|403)\b|unauthori[sz]ed|forbidden|invalid\s*(api\s*)?key|authentication|token.*(invalid|expired)/i },
  { kind: 'quota', pattern: /quota|insufficient|余额|额度|credit|billing.*limit|exhausted/i },
  { kind: 'rate_limit', pattern: /\b429\b|rate.?limit|too many requests|限流/i },
  { kind: 'model', pattern: /model.*(not found|unavailable|unsupported|does not exist)|unknown model|模型.*(不存在|不可用)/i },
  { kind: 'unreachable', pattern: /timeout|timed out|connection refused|dns|network|unreachable|connect error|无法连接/i },
  { kind: 'upstream', pattern: /\b5\d\d\b|upstream.*(error|failed)|bad gateway|service unavailable/i },
  { kind: 'protocol', pattern: /invalid (json|response)|decode|schema|protocol|unexpected (body|response)|兼容/i },
];

export function classifyFailureText(text: string): ChannelFailureKind {
  return rules.find(({ pattern }) => pattern.test(text))?.kind ?? 'unknown';
}

export function classifyChannelFailure(
  channel: Pick<Channel, 'errorMessage' | 'providerQuotaStatus' | 'disabledAPIKeys'>
): ChannelFailureEvidence | null {
  const quotaStatus = channel.providerQuotaStatus?.status?.trim().toLowerCase();
  const quotaEvidence = quotaStatus && ['warning', 'exhausted', 'unknown'].includes(quotaStatus) ? quotaStatus : null;
  const disabledKeyEvidence = (channel.disabledAPIKeys ?? []).map((key) => [key.errorCode, key.reason].filter(Boolean).join(' '));
  const evidence = [channel.errorMessage, quotaEvidence, ...disabledKeyEvidence].filter((value): value is string => Boolean(value?.trim()));

  if (evidence.length === 0) return null;

  if (channel.errorMessage) {
    const kind = classifyFailureText(channel.errorMessage);
    if (kind !== 'unknown') return { kind, evidence };
  }
  if (quotaStatus === 'warning' || quotaStatus === 'exhausted') return { kind: 'quota', evidence };
  for (const candidate of disabledKeyEvidence) {
    const kind = classifyFailureText(candidate);
    if (kind !== 'unknown') return { kind, evidence };
  }
  return { kind: 'unknown', evidence };
}
