function normalizeBasePath(value: string | null | undefined): string {
  const trimmed = value?.trim() ?? '';
  if (!trimmed || trimmed === '/') return '';
  const withLeadingSlash = trimmed.startsWith('/') ? trimmed : `/${trimmed}`;
  return withLeadingSlash.replace(/\/+$/, '');
}

function runtimeBasePath(): string {
  if (typeof document === 'undefined') return '';
  const configured = document.querySelector<HTMLMetaElement>('meta[name="conduit-base-path"]')?.content;
  return normalizeBasePath(configured);
}

/** Base path injected into the served SPA index by Conduit API. */
export const APP_BASE_PATH = runtimeBasePath();

/** Prefix a same-origin absolute path without double-prefixing it. */
export function withBasePath(path: string): string {
  if (!path.startsWith('/') || !APP_BASE_PATH) return path;
  if (path === APP_BASE_PATH || path.startsWith(`${APP_BASE_PATH}/`)) return path;
  return `${APP_BASE_PATH}${path}`;
}

/** Remove the configured mount prefix from a browser pathname. */
export function withoutBasePath(path: string): string {
  if (!APP_BASE_PATH) return path;
  if (path === APP_BASE_PATH) return '/';
  if (path.startsWith(`${APP_BASE_PATH}/`)) return path.slice(APP_BASE_PATH.length);
  return path;
}
