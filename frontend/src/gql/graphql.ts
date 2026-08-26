import { toast } from 'sonner';
import { getTokenFromStorage, removeTokenFromStorage } from '@/stores/authStore';
import { getProjectIdFromStorage } from '@/stores/projectStore';
import i18n from '@/lib/i18n';

export class GraphQLRequestError extends Error {
  status?: number;
  isAuthError: boolean;
  extensions?: Record<string, any>;

  constructor(message: string, options?: { status?: number; isAuthError?: boolean; extensions?: Record<string, any> }) {
    super(message);
    this.name = 'GraphQLRequestError';
    this.status = options?.status;
    this.isAuthError = Boolean(options?.isAuthError);
    this.extensions = options?.extensions;
  }
}

export function isAuthError(error: unknown): error is GraphQLRequestError {
  return error instanceof GraphQLRequestError && error.isAuthError;
}

// Utility function to extract the operation name from a GraphQL query string
export function extractOperationName(query: string): string | undefined {
  // Remove leading whitespace and match the operation name from GraphQL query/mutation/subscription
  // Pattern: (query|mutation|subscription)\s+Name
  const trimmedQuery = query.trim();
  const operationMatch = trimmedQuery.match(/^(query|mutation|subscription)\s+(\w+)/i);
  if (operationMatch) {
    return operationMatch[2]; // Return the operation name
  }
  return undefined;
}

export const GRAPHQL_ENDPOINT = '/admin/graphql';
let authRedirectStarted = false;

export function expireSessionAndRedirect() {
  removeTokenFromStorage();
  if (authRedirectStarted || window.location.pathname === '/sign-in') return;
  authRedirectStarted = true;
  toast.error(i18n.t('common.errors.sessionExpiredSignIn'));
  // A document-level navigation also replaces a stale Vite bundle after a
  // backend restart or deployment. Client-side routing can otherwise try to
  // import chunk names from the previous build and render the generic 500 UI.
  window.location.replace('/sign-in');
}

function isForbiddenGraphQLError(error: any): boolean {
  return error?.extensions?.code === 'FORBIDDEN' || error?.message?.toLowerCase().includes('permission denied');
}

export function isUnauthorizedGraphQLError(error: any): boolean {
  return error?.extensions?.code === 'UNAUTHENTICATED';
}

// GraphQL client function with token support
export async function graphqlRequest<T>(
  query: string,
  variables?: Record<string, any>,
  customHeaders?: Record<string, string>
): Promise<T> {
  // Get token from localStorage
  const token = getTokenFromStorage();

  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
  };

  // Add Authorization header if token exists
  if (token) {
    headers['Authorization'] = `Bearer ${token}`;
  }

  // Keep every admin GraphQL request bound to the Project selected in the
  // application shell. Feature-specific callers may still override it below.
  const projectId = getProjectIdFromStorage();
  if (projectId) {
    headers['X-Project-ID'] = projectId;
  }

  // Merge custom headers
  if (customHeaders) {
    Object.assign(headers, customHeaders);
  }

  // Extract operation name from the query for tracing
  const operationName = extractOperationName(query);

  let response: Response;

  try {
    response = await fetch(GRAPHQL_ENDPOINT, {
      method: 'POST',
      headers,
      body: JSON.stringify({
        query,
        variables,
        operationName, // Add operation name for tracing
      }),
    });
  } catch (_error) {
    throw new GraphQLRequestError('Network error', { status: undefined, isAuthError: false });
  }

  // Handle explicit auth failures (401 only — 403 is a permission denial, not a session issue)
  if (response.status === 401) {
    expireSessionAndRedirect();
    throw new GraphQLRequestError('Unauthorized', { status: response.status, isAuthError: true });
  }

  // Handle permission denial — do NOT clear token or redirect
  if (response.status === 403) {
    throw new GraphQLRequestError('Forbidden', { status: 403, isAuthError: false });
  }

  // Check content type before parsing JSON
  const contentType = response.headers.get('content-type');
  if (!contentType || !contentType.includes('application/json')) {
    throw new GraphQLRequestError('Server returned non-JSON response. Please check the API endpoint.', {
      status: response.status,
    });
  }

  let result;
  try {
    result = await response.json();
  } catch (_error) {
    throw new GraphQLRequestError('Failed to parse server response as JSON', {
      status: response.status,
    });
  }

  if (!response.ok) {
    throw new GraphQLRequestError(result?.errors?.[0]?.message || 'Request failed', {
      status: response.status,
    });
  }

  if (result.errors) {
    const firstError = result.errors[0];

    // Check for authentication errors
    const authError = result.errors.find(isUnauthorizedGraphQLError);

    if (authError) {
      expireSessionAndRedirect();
      throw new GraphQLRequestError('Unauthorized', { status: 401, isAuthError: true });
    }

    throw new GraphQLRequestError(firstError?.message || 'GraphQL Error', {
      status: isForbiddenGraphQLError(firstError) ? 403 : undefined,
      extensions: firstError?.extensions,
    });
  }

  return result.data;
}
