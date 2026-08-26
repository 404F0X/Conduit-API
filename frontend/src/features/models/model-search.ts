export type ModelsCatalogView = 'models' | 'channels';

export type ModelsCatalogSearch = {
  view?: ModelsCatalogView;
  model?: string;
  channel?: string;
  upstreamModel?: string;
  deployment?: string;
};

function optionalString(value: unknown) {
  return typeof value === 'string' && value.trim() ? value : undefined;
}

export function validateModelsSearch(search: Record<string, unknown>): ModelsCatalogSearch {
  const view = search.view === 'models' || search.view === 'channels' ? search.view : undefined;
  const model = optionalString(search.model);
  const channel = optionalString(search.channel);
  const upstreamModel = optionalString(search.upstreamModel);
  const deployment = optionalString(search.deployment);
  const resolvedView = view || (model ? 'models' : channel ? 'channels' : undefined);
  return {
    ...(resolvedView ? { view: resolvedView } : {}),
    ...(resolvedView === 'models' && model ? { model } : {}),
    ...(resolvedView === 'channels' && channel ? { channel } : {}),
    ...(resolvedView === 'channels' && channel && upstreamModel ? { upstreamModel } : {}),
    ...(resolvedView === 'channels' && channel && deployment ? { deployment } : {}),
  };
}
