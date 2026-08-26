import { ChangeSetPage } from './change-set-page';
import type { ChangeSetRouteSearch } from './change-set-search';

export function ChangeSetWorkbenchPage({ initialFilters }: { initialFilters?: ChangeSetRouteSearch }) {
  return <ChangeSetPage mode='workbench' initialFilters={initialFilters} />;
}
