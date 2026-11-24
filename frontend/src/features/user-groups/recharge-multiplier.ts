export const DEFAULT_RECHARGE_PPM = '1000000';
export const MAX_RECHARGE_PPM = '1000000000';

/** Human multiplier (1.5×) to the server's positive integer PPM contract. */
export function rechargeMultiplierToPPM(input: string): string | null {
  const text = input.trim();
  if (text.length > 20 || !/^(0|[1-9]\d*)(?:\.\d{1,6})?$/.test(text)) return null;
  const [whole, fraction = ''] = text.split('.');
  const ppm = BigInt(whole) * 1000000n + BigInt(fraction.padEnd(6, '0'));
  return ppm > 0n && ppm <= BigInt(MAX_RECHARGE_PPM) ? ppm.toString() : null;
}

export function rechargePPMToMultiplier(ppm: string): string | null {
  if (!/^[1-9]\d*$/.test(ppm) || ppm.length > 10 || BigInt(ppm) > BigInt(MAX_RECHARGE_PPM)) return null;
  const digits = ppm.padStart(7, '0');
  const fraction = digits.slice(-6).replace(/0+$/, '');
  return fraction ? `${digits.slice(0, -6)}.${fraction}` : digits.slice(0, -6);
}
