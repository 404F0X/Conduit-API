import assert from 'node:assert/strict';
import test from 'node:test';
import { accessPlanIDsForEdit, mergeAccessPlanOptions, normalizeAccessPlanIDs, toggleAccessPlanID } from './access-plan-selection.ts';

test('create selection accumulates every chosen access plan', () => {
  const first = toggleAccessPlanID([], 'model-group-a');
  const second = toggleAccessPlanID(first, 'model-group-b');

  assert.deepEqual(second, ['model-group-a', 'model-group-b']);
  assert.deepEqual(normalizeAccessPlanIDs(second), ['model-group-a', 'model-group-b']);
});

test('edit initialization and toggles preserve unrelated access plans', () => {
  const initial = accessPlanIDsForEdit([
    { id: 'model-group-a', name: 'A' },
    { id: 'model-group-b', name: 'B' },
  ]);
  const withThird = toggleAccessPlanID(initial, 'model-group-c');
  const withoutSecond = toggleAccessPlanID(withThird, 'model-group-b');

  assert.deepEqual(initial, ['model-group-a', 'model-group-b']);
  assert.deepEqual(withThird, ['model-group-a', 'model-group-b', 'model-group-c']);
  assert.deepEqual(withoutSecond, ['model-group-a', 'model-group-c']);
});

test('known plan options survive a partial model-group query', () => {
  const options = mergeAccessPlanOptions(
    [
      { id: 'model-group-a', name: 'Existing A' },
      { id: 'model-group-b', name: 'Existing B' },
    ],
    [
      { id: 'model-group-b', name: 'Queried B' },
      { id: 'model-group-c', name: 'Queried C' },
    ]
  );

  assert.deepEqual(options, [
    { id: 'model-group-a', name: 'Existing A' },
    { id: 'model-group-b', name: 'Existing B' },
    { id: 'model-group-c', name: 'Queried C' },
  ]);
});
