import type { OperationsFlowRow, OperationsModelSeries, OperationsRouteHealth } from './data';

export type AnalyticsMetric = 'requests' | 'tokens' | 'revenue';
export type FlowStage = 'user' | 'project' | 'apiKey' | 'requestedModel' | 'actualModel' | 'channel';

export const FLOW_STAGES: FlowStage[] = ['user', 'project', 'apiKey', 'requestedModel', 'actualModel', 'channel'];

export type ModelMainChartMode = 'bar' | 'area';
export type ModelAnalysisMode = 'trend' | 'proportion' | 'top';
export interface ModelAnalyticsPreferences {
  periodDays: 1 | 7 | 14 | 29;
  mainChart: ModelMainChartMode;
  analysisMode: ModelAnalysisMode;
}
export const MODEL_ANALYTICS_STORAGE_KEY = 'conduit.operations.model-analytics.v1';
export const DEFAULT_MODEL_ANALYTICS_PREFERENCES: ModelAnalyticsPreferences = { periodDays: 1, mainChart: 'bar', analysisMode: 'trend' };

export function loadModelAnalyticsPreferences(storage?: Pick<Storage, 'getItem'>): ModelAnalyticsPreferences {
  if (!storage) return { ...DEFAULT_MODEL_ANALYTICS_PREFERENCES };
  try {
    const value = JSON.parse(storage.getItem(MODEL_ANALYTICS_STORAGE_KEY) ?? 'null') as Partial<ModelAnalyticsPreferences> | null;
    if (!value) return { ...DEFAULT_MODEL_ANALYTICS_PREFERENCES };
    return {
      periodDays: ([1, 7, 14, 29] as const).includes(value.periodDays as 1) ? (value.periodDays as 1 | 7 | 14 | 29) : 1,
      mainChart: value.mainChart === 'area' ? 'area' : 'bar',
      analysisMode: value.analysisMode === 'proportion' || value.analysisMode === 'top' ? value.analysisMode : 'trend',
    };
  } catch {
    return { ...DEFAULT_MODEL_ANALYTICS_PREFERENCES };
  }
}

function floorBucket(date: Date, granularity: OperationsModelSeries['granularity']) {
  const value = new Date(date);
  value.setUTCMinutes(0, 0, 0);
  if (granularity !== 'hour') value.setUTCHours(0);
  if (granularity === 'week') {
    const day = value.getUTCDay() || 7;
    value.setUTCDate(value.getUTCDate() - day + 1);
  }
  return value;
}

export interface ModelTimeChart {
  models: string[];
  rows: Array<Record<string, number | string>>;
  totals: Map<string, number>;
}

export function lastNonzeroBucketIndex(rows: ModelTimeChart['rows'], models: string[]) {
  for (let index = rows.length - 1; index >= 0; index -= 1) {
    const total = models.reduce((sum, model) => sum + Number(rows[index][model] ?? 0), 0);
    if (total > 0) return index;
  }
  return -1;
}

export function bucketScrollTarget(
  bucketIndex: number,
  bucketCount: number,
  contentWidth: number,
  viewportWidth: number,
  viewportRatio = 0.7,
  plotInsetStart = 48,
  plotInsetEnd = 16
) {
  if (bucketIndex < 0 || bucketCount <= 0 || contentWidth <= viewportWidth) return 0;
  const maximum = Math.max(0, contentWidth - viewportWidth);
  const plotWidth = Math.max(0, contentWidth - plotInsetStart - plotInsetEnd);
  const bucketCenter = plotInsetStart + plotWidth * ((bucketIndex + 0.5) / bucketCount);
  return Math.min(maximum, Math.max(0, bucketCenter - viewportWidth * viewportRatio));
}

export function buildModelTimeChart(series: OperationsModelSeries, metric: AnalyticsMetric, topN: number): ModelTimeChart {
  const valueOf = (point: OperationsModelSeries['points'][number]) =>
    metric === 'requests' ? point.meteredRequests : metric === 'tokens' ? point.totalTokens : (point.recognizedUsageRevenue ?? 0);
  const totals = new Map<string, number>();
  for (const point of series.points) totals.set(point.requestedModel, (totals.get(point.requestedModel) ?? 0) + valueOf(point));
  const ranked = [...totals.entries()].sort((a, b) => b[1] - a[1]);
  const models = ranked.slice(0, topN).map(([model]) => model);
  const hasOther = ranked.length > topN;
  if (hasOther) models.push('__other__');
  const start = floorBucket(new Date(series.periodStart), series.granularity);
  const end = new Date(series.periodEnd);
  const step = series.granularity === 'hour' ? 3_600_000 : series.granularity === 'day' ? 86_400_000 : 604_800_000;
  const rows = new Map<string, Record<string, number | string>>();
  for (let cursor = start.getTime(); cursor <= end.getTime(); cursor += step) {
    const key = new Date(cursor).toISOString();
    const row: Record<string, number | string> = { bucketStart: key };
    for (const model of models) row[model] = 0;
    rows.set(key, row);
  }
  for (const point of series.points) {
    const key = floorBucket(new Date(point.bucketStart), series.granularity).toISOString();
    const row = rows.get(key) ?? { bucketStart: key };
    const model = models.includes(point.requestedModel) ? point.requestedModel : hasOther ? '__other__' : point.requestedModel;
    row[model] = Number(row[model] ?? 0) + valueOf(point);
    rows.set(key, row);
  }
  return { models, rows: [...rows.values()].sort((a, b) => String(a.bucketStart).localeCompare(String(b.bucketStart))), totals };
}

export function metricValue(row: OperationsFlowRow, metric: AnalyticsMetric) {
  if (metric === 'tokens') return row.totalTokens;
  if (metric === 'revenue') return row.recognizedUsageRevenue ?? 0;
  return row.meteredRequests;
}

function identity(value: string, id: number | null) {
  return value.trim() || (id == null ? '' : `#${id}`);
}

export function stageValue(row: OperationsFlowRow, stage: FlowStage) {
  if (stage === 'user') return identity(row.userEmail, row.userId);
  if (stage === 'project') return identity(row.projectName, row.projectId);
  if (stage === 'apiKey') return identity(row.apiKeyName, row.apiKeyId);
  if (stage === 'requestedModel') return row.requestedModel.trim();
  if (stage === 'actualModel') return row.actualModel.trim();
  return identity(row.channelName, row.channelId);
}

export interface ModelAnalyticsRow {
  requestedModel: string;
  requests: number;
  tokens: number;
  cost: number;
  revenue: number;
  costComplete: boolean;
  revenueComplete: boolean;
  supplies: Array<{
    actualModel: string;
    channelId: number | null;
    channelName: string;
    requests: number;
    tokens: number;
    attempts: number | null;
    successRate: number | null;
  }>;
}

export function aggregateModels(rows: OperationsFlowRow[], routeHealth: OperationsRouteHealth[]): ModelAnalyticsRow[] {
  const models = new Map<string, ModelAnalyticsRow>();
  for (const row of rows) {
    const requestedModel = row.requestedModel.trim();
    let model = models.get(requestedModel);
    if (!model) {
      model = {
        requestedModel,
        requests: 0,
        tokens: 0,
        cost: 0,
        revenue: 0,
        costComplete: true,
        revenueComplete: true,
        supplies: [],
      };
      models.set(requestedModel, model);
    }
    model.requests += row.meteredRequests;
    model.tokens += row.totalTokens;
    model.cost += row.recordedUpstreamCost ?? 0;
    model.revenue += row.recognizedUsageRevenue ?? 0;
    model.costComplete &&= row.recordedUpstreamCost != null;
    model.revenueComplete &&= row.recognizedUsageRevenue != null;
    const actualModel = row.actualModel.trim();
    let supply = model.supplies.find((item) => item.actualModel === actualModel && item.channelId === row.channelId);
    if (!supply) {
      const health = routeHealth.find((item) => item.actualModel === actualModel && item.channelId === row.channelId);
      supply = {
        actualModel,
        channelId: row.channelId,
        channelName: row.channelName.trim(),
        requests: 0,
        tokens: 0,
        attempts: health?.upstreamAttempts ?? null,
        successRate: health?.successRate ?? null,
      };
      model.supplies.push(supply);
    }
    supply.requests += row.meteredRequests;
    supply.tokens += row.totalTokens;
  }
  return [...models.values()].sort((a, b) => b.requests - a.requests);
}

export interface UserAnalyticsRow {
  key: string;
  userId: number | null;
  email: string;
  projects: number;
  apiKeys: number;
  models: number;
  channels: number;
  requests: number;
  tokens: number;
  cost: number;
  revenue: number;
  costComplete: boolean;
  revenueComplete: boolean;
  lastActivityAt: string | null;
}

export function aggregateUsers(rows: OperationsFlowRow[]): UserAnalyticsRow[] {
  const groups = new Map<
    string,
    UserAnalyticsRow & { projectSet: Set<string>; keySet: Set<string>; modelSet: Set<string>; channelSet: Set<string> }
  >();
  for (const row of rows) {
    const key = row.userId == null ? `email:${row.userEmail.trim()}` : `id:${row.userId}`;
    let user = groups.get(key);
    if (!user) {
      user = {
        key,
        userId: row.userId,
        email: row.userEmail.trim(),
        projects: 0,
        apiKeys: 0,
        models: 0,
        channels: 0,
        requests: 0,
        tokens: 0,
        cost: 0,
        revenue: 0,
        costComplete: true,
        revenueComplete: true,
        lastActivityAt: null,
        projectSet: new Set(),
        keySet: new Set(),
        modelSet: new Set(),
        channelSet: new Set(),
      };
      groups.set(key, user);
    }
    user.projectSet.add(`${row.projectId ?? ''}:${row.projectName}`);
    user.keySet.add(`${row.apiKeyId ?? ''}:${row.apiKeyName}`);
    user.modelSet.add(row.requestedModel);
    user.channelSet.add(`${row.channelId ?? ''}:${row.channelName}`);
    user.requests += row.meteredRequests;
    user.tokens += row.totalTokens;
    user.cost += row.recordedUpstreamCost ?? 0;
    user.revenue += row.recognizedUsageRevenue ?? 0;
    user.costComplete &&= row.recordedUpstreamCost != null;
    user.revenueComplete &&= row.recognizedUsageRevenue != null;
    if (row.lastActivityAt && (!user.lastActivityAt || row.lastActivityAt > user.lastActivityAt)) user.lastActivityAt = row.lastActivityAt;
  }
  return [...groups.values()]
    .map(({ projectSet, keySet, modelSet, channelSet, ...user }) => ({
      ...user,
      projects: projectSet.size,
      apiKeys: keySet.size,
      models: modelSet.size,
      channels: channelSet.size,
    }))
    .sort((a, b) => b.requests - a.requests);
}

export interface FlowPath {
  key: string;
  values: Array<{ stage: FlowStage; value: string }>;
  metric: number;
  rows: number;
  other?: boolean;
}

export type FlowOverflowMode = 'merge' | 'hide';

export function maskSensitiveLabel(value: string, stage: FlowStage, enabled: boolean) {
  if (!enabled || value === '__other__' || (stage !== 'user' && stage !== 'apiKey')) return value;
  if (stage === 'apiKey') return value.length <= 4 ? '••••' : `•••• ${value.slice(-4)}`;
  const at = value.indexOf('@');
  if (at > 0) return `${value.slice(0, Math.min(2, at))}***${value.slice(at)}`;
  return value.length <= 2 ? '•••' : `${value.slice(0, 2)}***`;
}

export function toggleFlowStage(stages: FlowStage[], stage: FlowStage) {
  if (stages.includes(stage)) return stages.length <= 2 ? stages : stages.filter((item) => item !== stage);
  return FLOW_STAGES.filter((item) => item === stage || stages.includes(item));
}

export function buildFlowPaths(
  rows: OperationsFlowRow[],
  stages: FlowStage[],
  metric: AnalyticsMetric,
  limit: number,
  overflowMode: FlowOverflowMode = 'merge'
): FlowPath[] {
  const selected = FLOW_STAGES.filter((stage) => stages.includes(stage));
  const paths = new Map<string, FlowPath>();
  for (const row of rows) {
    const values = selected.map((stage) => ({ stage, value: stageValue(row, stage) }));
    const key = values.map((item) => `${item.stage}:${item.value}`).join('|');
    const current = paths.get(key) ?? { key, values, metric: 0, rows: 0 };
    current.metric += metricValue(row, metric);
    current.rows += 1;
    paths.set(key, current);
  }
  const ordered = [...paths.values()].sort((a, b) => b.metric - a.metric);
  const visible = ordered.slice(0, limit);
  const omitted = ordered.slice(limit);
  if (omitted.length && overflowMode === 'merge') {
    visible.push({
      key: '__other__',
      values: selected.map((stage) => ({ stage, value: '__other__' })),
      metric: omitted.reduce((total, path) => total + path.metric, 0),
      rows: omitted.reduce((total, path) => total + path.rows, 0),
      other: true,
    });
  }
  return visible;
}

export interface SankeyGraph {
  nodes: Array<{ name: string; stage: FlowStage; key: string }>;
  links: Array<{ source: number; target: number; value: number }>;
}

export function buildSankeyGraph(paths: FlowPath[], maskSensitive: boolean): SankeyGraph {
  const nodes: SankeyGraph['nodes'] = [];
  const nodeIndexes = new Map<string, number>();
  const links = new Map<string, SankeyGraph['links'][number]>();
  const nodeIndex = (stage: FlowStage, value: string) => {
    const key = `${stage}:${value}`;
    const existing = nodeIndexes.get(key);
    if (existing != null) return existing;
    const index = nodes.length;
    nodes.push({ name: maskSensitiveLabel(value, stage, maskSensitive), stage, key });
    nodeIndexes.set(key, index);
    return index;
  };
  for (const path of paths) {
    if (path.metric <= 0) continue;
    for (let index = 0; index < path.values.length - 1; index += 1) {
      const sourceValue = path.values[index];
      const targetValue = path.values[index + 1];
      const source = nodeIndex(sourceValue.stage, sourceValue.value);
      const target = nodeIndex(targetValue.stage, targetValue.value);
      const key = `${source}:${target}`;
      const link = links.get(key) ?? { source, target, value: 0 };
      link.value += path.metric;
      links.set(key, link);
    }
  }
  return { nodes, links: [...links.values()] };
}
