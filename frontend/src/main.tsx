import { StrictMode } from 'react';
import ReactDOM from 'react-dom/client';
import { QueryCache, QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createRouter } from '@tanstack/react-router';
import { expireSessionAndRedirect } from '@/gql/graphql';
import { toast } from 'sonner';
import { useAuthStore } from '@/stores/authStore';
import { handleServerError } from '@/utils/handle-server-error';
import { FontProvider } from './context/font-context';
import { SearchProvider } from './context/search-context';
import { ThemeProvider } from './context/theme-context';
import './index.css';
// Initialize i18n
import './lib/i18n';
import i18n from './lib/i18n';
// Generated Routes
import { routeTree } from './routeTree.gen';

// Vite emits this event when a tab still references a lazy chunk from an
// earlier deployment. Reload the current document once so it receives the
// current no-cache index and route manifest instead of showing a false 500.
window.addEventListener('vite:preloadError', (event) => {
  const key = 'conduit:last-stale-chunk-reload';
  const now = Date.now();
  const lastReload = Number(sessionStorage.getItem(key) || 0);
  if (now - lastReload > 15_000) {
    // Prevent Vite from rejecting this import only when we are actually
    // replacing the document. Preventing a rate-limited error without a
    // reload makes the import resolve to `undefined`, which later crashes
    // TanStack Router while it reads the route module's `component` export.
    event.preventDefault();
    sessionStorage.setItem(key, String(now));
    window.location.reload();
  }
});

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: (failureCount, error) => {
        // eslint-disable-next-line no-console
        if (import.meta.env.DEV) console.log({ failureCount, error });

        if (failureCount >= 0 && import.meta.env.DEV) return false;
        if (failureCount > 3 && import.meta.env.PROD) return false;

        // For fetch API errors, we check if it's a Response object with status
        const status =
          error instanceof Response ? error.status : error && typeof error === 'object' && 'status' in error ? (error as any).status : 0;

        return ![401, 403, 422].includes(status);
      },
      refetchOnWindowFocus: import.meta.env.PROD,
      staleTime: 10 * 1000, // 10s
    },
    mutations: {
      onError: (error) => {
        handleServerError(error);

        // For fetch API errors, we check if it's a Response object with status
        const status =
          error instanceof Response ? error.status : error && typeof error === 'object' && 'status' in error ? (error as any).status : 0;

        if (status === 304) {
          toast.error(i18n.t('common.errors.contentNotModified'));
        }
      },
    },
  },
  queryCache: new QueryCache({
    onError: (error) => {
      // For fetch API errors, we check if it's a Response object with status
      const status =
        error instanceof Response ? error.status : error && typeof error === 'object' && 'status' in error ? (error as any).status : 0;

      if (status === 401) {
        useAuthStore.getState().auth.reset();
        expireSessionAndRedirect();
        return;
      }
      if (status === 500) {
        toast.error(i18n.t('common.errors.internalServerError'));
        // router.navigate({ to: '/500' })
      }
    },
  }),
});

// Create a new router instance
const router = createRouter({
  routeTree,
  context: { queryClient },
  defaultPreload: 'intent',
  defaultPreloadStaleTime: 0,
});

// Register the router instance for type safety
declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router;
  }
}

// Render the app
const rootElement = document.getElementById('root')!;
if (!rootElement.innerHTML) {
  const root = ReactDOM.createRoot(rootElement);
  root.render(
    <StrictMode>
      <QueryClientProvider client={queryClient}>
        <ThemeProvider defaultTheme='system' defaultColorScheme='claude'>
          <FontProvider>
            <SearchProvider>
              <RouterProvider router={router} />
            </SearchProvider>
          </FontProvider>
        </ThemeProvider>
      </QueryClientProvider>
    </StrictMode>
  );
}
