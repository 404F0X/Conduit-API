import { ChangeSetPage } from './change-set-page';
import type { ChangeSetRouteSearch } from './change-set-search';

export function ChangelogPage({ initialFilters }: { initialFilters?: ChangeSetRouteSearch }) {
  return <ChangeSetPage mode='changelog' initialFilters={initialFilters} />;
}
