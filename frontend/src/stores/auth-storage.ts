export const ACCESS_TOKEN = 'conduit_access_token';
const USER_INFO = 'conduit_user_info';

function storage(persistent: boolean): Storage | undefined {
  try {
    return persistent ? globalThis.localStorage : globalThis.sessionStorage;
  } catch {
    return undefined;
  }
}

function read(persistent: boolean, key: string): string | null {
  try {
    return storage(persistent)?.getItem(key) ?? null;
  } catch {
    return null;
  }
}

function remove(persistent: boolean, key: string) {
  try {
    storage(persistent)?.removeItem(key);
  } catch {
    // Authentication still works in memory when browser storage is unavailable.
  }
}

export function getTokenFromStorage(): string {
  return read(false, ACCESS_TOKEN) || read(true, ACCESS_TOKEN) || '';
}

export function removeTokenFromStorage(): void {
  for (const persistent of [false, true]) {
    remove(persistent, ACCESS_TOKEN);
    remove(persistent, USER_INFO);
  }
}

export function setTokenToStorage(token: string, persistent = true): void {
  removeTokenFromStorage();
  try {
    storage(persistent)?.setItem(ACCESS_TOKEN, token);
  } catch {
    // Keep the live session in the auth store.
  }
}

export function getUserFromStorage<T>(): T | null {
  try {
    const persistent = !read(false, ACCESS_TOKEN);
    if (!read(persistent, ACCESS_TOKEN)) return null;
    const value = read(persistent, USER_INFO);
    return value ? (JSON.parse(value) as T) : null;
  } catch {
    return null;
  }
}

export function setUserToStorage(user: unknown): void {
  const persistent = !read(false, ACCESS_TOKEN);
  remove(!persistent, USER_INFO);
  try {
    if (user && read(persistent, ACCESS_TOKEN)) {
      storage(persistent)?.setItem(USER_INFO, JSON.stringify(user));
    } else {
      remove(persistent, USER_INFO);
    }
  } catch {
    // Keep the live session in the auth store.
  }
}
