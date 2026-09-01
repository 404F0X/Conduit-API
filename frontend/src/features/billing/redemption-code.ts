const CREDIT_AMOUNT_PATTERN = /^\d+(?:\.\d{1,6})?$/;
export const MAX_REDEMPTION_CODE_LENGTH = 128;

export function normalizeRedemptionCode(value: string): string {
  return value.trim();
}

export function canRedeemCode(value: string): boolean {
  const normalized = normalizeRedemptionCode(value);
  return normalized.length > 0 && normalized.length <= MAX_REDEMPTION_CODE_LENGTH;
}

export function isValidCreditAmount(value: string): boolean {
  const normalized = value.trim();
  return CREDIT_AMOUNT_PATTERN.test(normalized) && !/^0(?:\.0+)?$/.test(normalized);
}

export function isValidRedemptionQuantity(value: number): boolean {
  return Number.isInteger(value) && value >= 1 && value <= 1000;
}

export function isValidRedemptionUseLimit(value: number): boolean {
  return Number.isInteger(value) && value >= 1 && value <= 100_000;
}
