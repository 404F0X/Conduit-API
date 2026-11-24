const UNSIGNED_INTEGER = /^(0|[1-9]\d*)$/;
export const MAX_PAYMENT_MINOR = '9223372036854775807';

/** Converts decimal CNY to exact fen. No rounding, exponent or binary float. */
export function cnyToMinor(input: string, minimum = '1', maximum = MAX_PAYMENT_MINOR): string | null {
  const text = input.trim();
  if (text.length > 24 || !/^(0|[1-9]\d*)(?:\.\d{1,2})?$/.test(text)) return null;
  if (!UNSIGNED_INTEGER.test(minimum) || !UNSIGNED_INTEGER.test(maximum)) return null;
  const [whole, fraction = ''] = text.split('.');
  const minor = BigInt(whole) * 100n + BigInt(fraction.padEnd(2, '0'));
  const lower = BigInt(minimum),
    upper = BigInt(maximum);
  if (minor < 1n || lower < 1n || upper < lower || upper > BigInt(MAX_PAYMENT_MINOR) || minor < lower || minor > upper) return null;
  return minor.toString();
}

export function formatPaymentUnits(value: string, scale: number, fixed = false): string {
  if (!UNSIGNED_INTEGER.test(value) || !Number.isInteger(scale) || scale < 0 || scale > 6) return '—';
  if (!scale) return value;
  const padded = value.padStart(scale + 1, '0');
  const whole = padded.slice(0, -scale);
  const fraction = fixed ? padded.slice(-scale) : padded.slice(-scale).replace(/0+$/, '');
  return fraction ? `${whole}.${fraction}` : whole;
}

/** A returned URL is a destination, never evidence of payment. */
export function paymentCheckoutHref(value: string | null): string | null {
  if (!value) return null;
  try {
    const url = new URL(value);
    return url.protocol === 'https:' && !url.username && !url.password ? url.href : null;
  } catch {
    return null;
  }
}
