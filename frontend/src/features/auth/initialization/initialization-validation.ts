export interface InitializationPasswordFields {
  ownerPassword: string;
  confirmOwnerPassword: string;
}

export function initializationPasswordsMatch(values: InitializationPasswordFields): boolean {
  return values.ownerPassword === values.confirmOwnerPassword;
}
