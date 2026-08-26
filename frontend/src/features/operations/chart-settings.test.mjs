import assert from 'node:assert/strict';
import test from 'node:test';
import {
  DEFAULT_OPERATIONS_TREND_SERIES,
  OPERATIONS_TREND_STORAGE_KEY,
  hasVisibleTrendAxis,
  loadOperationsTrendSeries,
  parseOperationsTrendSeries,
} from './chart-settings.ts';

test('trend series settings preserve valid selections, including an empty selection', () => {
  assert.deepEqual(parseOperationsTrendSeries('["customerRequests","grossProfit"]'), ['customerRequests', 'grossProfit']);
  assert.deepEqual(parseOperationsTrendSeries('[]'), []);
});

test('trend series settings discard obsolete values without duplicating known values', () => {
  assert.deepEqual(parseOperationsTrendSeries('["grossProfit","removedSeries","grossProfit","failureRate"]'), [
    'grossProfit',
    'failureRate',
  ]);
});

test('trend series settings fall back safely for malformed or entirely obsolete values', () => {
  assert.deepEqual(parseOperationsTrendSeries('{broken'), DEFAULT_OPERATIONS_TREND_SERIES);
  assert.deepEqual(parseOperationsTrendSeries('["removedSeries"]'), DEFAULT_OPERATIONS_TREND_SERIES);
  assert.deepEqual(parseOperationsTrendSeries('{"grossProfit":true}'), DEFAULT_OPERATIONS_TREND_SERIES);
});

test('trend series settings tolerate unavailable browser storage', () => {
  assert.deepEqual(loadOperationsTrendSeries(), DEFAULT_OPERATIONS_TREND_SERIES);
  assert.deepEqual(
    loadOperationsTrendSeries({
      getItem(key) {
        assert.equal(key, OPERATIONS_TREND_STORAGE_KEY);
        throw new Error('storage unavailable');
      },
    }),
    DEFAULT_OPERATIONS_TREND_SERIES
  );
});

test('axis visibility follows the exact selected series', () => {
  const selected = new Set(['grossProfit', 'requestFailureRate']);
  assert.equal(hasVisibleTrendAxis(selected, ['recordedUpstreamCost', 'grossProfit']), true);
  assert.equal(hasVisibleTrendAxis(selected, ['customerRequests', 'retryCount']), false);
});
