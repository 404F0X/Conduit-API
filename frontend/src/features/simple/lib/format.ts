/**
 * 简单模式页面统一的数字、金额、时间写法（见 UI 信息规格“全站通用规则”）。
 * - 金额两位小数；小于 0.01 写“不到 0.01”，悬停看精确值。
 * - Token 数：中文用 万 / 亿，其他语言用 K / M / B。
 * - 时间：列表里写相对时间，悬停看完整时间。
 */

type Translate = (key: string, options?: Record<string, unknown>) => string;

function toNumber(value: string | number | null | undefined): number | null {
  if (value === null || value === undefined || value === '') return null;
  const parsed = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

/** 金额：返回显示文本和悬停用的精确值。label 是站点展示单位（例如 ¥ 或站长自定义的积分名）。 */
export function formatMoney(label: string, value: string | number | null | undefined, t: Translate): { text: string; exact: string } {
  const amount = toNumber(value);
  if (amount === null) return { text: '—', exact: '' };
  const exact = `${label} ${typeof value === 'string' ? value : String(amount)}`;
  if (amount !== 0 && Math.abs(amount) < 0.01) {
    return { text: t('simple.format.lessThanCent', { label }), exact };
  }
  const fixed = amount.toFixed(2).replace(/\.00$/, '');
  return { text: `${label} ${fixed}`, exact };
}

/** Token 等计数：1.2万 / 3.4亿，或 1.2K / 3.4M。 */
export function formatCount(value: number | null | undefined, language: string): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return '0';
  const abs = Math.abs(value);
  const trim = (n: number) => n.toFixed(1).replace(/\.0$/, '');
  if (language.startsWith('zh')) {
    if (abs >= 100_000_000) return `${trim(value / 100_000_000)}亿`;
    if (abs >= 10_000) return `${trim(value / 10_000)}万`;
    return value.toLocaleString('zh-CN');
  }
  if (abs >= 1_000_000_000) return `${trim(value / 1_000_000_000)}B`;
  if (abs >= 1_000_000) return `${trim(value / 1_000_000)}M`;
  if (abs >= 1_000) return `${trim(value / 1_000)}K`;
  return value.toLocaleString('en-US');
}

export function formatFullTime(value: Date | string | null | undefined, language: string): string {
  if (!value) return '';
  const date = value instanceof Date ? value : new Date(value);
  if (Number.isNaN(date.getTime())) return '';
  return new Intl.DateTimeFormat(language, { dateStyle: 'medium', timeStyle: 'medium' }).format(date);
}

/** 相对时间：刚刚 / 3 分钟前 / 2 小时前 / 昨天 14:20 / 11月3日 */
export function formatRelativeTime(value: Date | string | null | undefined, language: string, t: Translate): string {
  if (!value) return '—';
  const date = value instanceof Date ? value : new Date(value);
  if (Number.isNaN(date.getTime())) return '—';
  const diffMs = Date.now() - date.getTime();
  const minutes = Math.floor(diffMs / 60_000);
  if (diffMs >= 0 && minutes < 1) return t('simple.format.justNow');
  if (diffMs >= 0 && minutes < 60) return t('simple.format.minutesAgo', { count: minutes });
  const hours = Math.floor(minutes / 60);
  if (diffMs >= 0 && hours < 24) return t('simple.format.hoursAgo', { count: hours });
  const time = new Intl.DateTimeFormat(language, { hour: '2-digit', minute: '2-digit' }).format(date);
  const yesterday = new Date();
  yesterday.setDate(yesterday.getDate() - 1);
  if (date.toDateString() === yesterday.toDateString()) return t('simple.format.yesterdayAt', { time });
  const sameYear = date.getFullYear() === new Date().getFullYear();
  return new Intl.DateTimeFormat(language, sameYear ? { month: 'short', day: 'numeric' } : { dateStyle: 'medium' }).format(date);
}

/** 毫秒时长：850 毫秒 / 1.2 秒 */
export function formatDuration(ms: number | null | undefined, t: Translate): string {
  if (ms === null || ms === undefined || !Number.isFinite(ms)) return '—';
  if (ms < 1000) return t('simple.format.milliseconds', { value: Math.round(ms) });
  return t('simple.format.seconds', { value: (ms / 1000).toFixed(1).replace(/\.0$/, '') });
}

/** 今天零点（本地时区） */
export function startOfToday(): Date {
  const date = new Date();
  date.setHours(0, 0, 0, 0);
  return date;
}

/** 本自然月第一天零点（本地时区） */
export function startOfMonth(): Date {
  const date = startOfToday();
  date.setDate(1);
  return date;
}

/** 站点对外的接入地址：取不到站点公网地址时用浏览器当前地址，不再退到 localhost。 */
export function gatewayBaseUrl(publicUrl?: string | null): string {
  const base = (publicUrl || '').trim().replace(/\/+$/, '');
  if (base) return `${base}/v1`;
  if (typeof window !== 'undefined') return `${window.location.origin}/v1`;
  return '/v1';
}
