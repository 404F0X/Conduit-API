import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';

const componentsRoot = join(import.meta.dirname, 'components');
const readComponent = (name) => readFileSync(join(componentsRoot, name), 'utf8');

const chartComponents = [
  'daily-requests-stats.tsx',
  'fastest-performers-card.tsx',
  'performance-chart.tsx',
  'requests-by-api-key-chart.tsx',
  'requests-by-channel-chart.tsx',
  'requests-by-model-chart.tsx',
  'tokens-by-api-key-chart.tsx',
  'tokens-by-channel-chart.tsx',
  'tokens-by-model-chart.tsx',
];

test('dashboard chart series render without expensive mount animations', () => {
  for (const file of chartComponents) {
    const source = readComponent(file);
    const series = source.match(/<(?:Area|Bar)\b[^>]*>/gs) ?? [];

    assert.ok(series.length > 0, `${file} must contain at least one chart series`);
    for (const openingTag of series) {
      assert.match(openingTag, /isAnimationActive=\{false\}/, `${file} has an animated chart series: ${openingTag}`);
    }
  }
});

test('the live requests indicator does not run an infinite decorative animation', () => {
  const source = readComponent('today-requests-card.tsx');

  assert.doesNotMatch(source, /animate-(?:ping|pulse|spin)/);
});
