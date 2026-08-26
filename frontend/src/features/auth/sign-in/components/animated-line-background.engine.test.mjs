import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import ts from 'typescript';

const source = readFileSync(join(import.meta.dirname, 'animated-line-background.engine.ts'), 'utf8');
const output = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 },
}).outputText;
const engineModule = { exports: {} };
new Function('require', 'module', 'exports', output)(() => ({}), engineModule, engineModule.exports);

const {
  advanceFrameTiming,
  animationConfig,
  getFormBounds,
  getParticleCount,
  initParticles,
  renderParticles,
  selectParticleConnections,
  shouldAnimateCanvas,
} = engineModule.exports;

const boundsOutsideCanvas = {
  formCenterX: 12_500,
  formCenterY: 12_500,
  formLeft: 12_000,
  formRight: 13_000,
  formTop: 12_000,
  formBottom: 13_000,
};

const createDenseParticles = (count) =>
  Array.from({ length: count }, (_, index) => ({
    x: 20 + (index % 16) * 2,
    y: 20 + (Math.floor(index / 16) % 16) * 2,
    xa: 0.1,
    ya: -0.1,
    max: 20_000,
  }));

test('uses explicit mobile, desktop, and wide particle budgets', () => {
  assert.equal(getParticleCount(375), 0);
  assert.equal(getParticleCount(animationConfig.minAnimatedViewportWidth - 1), 0);
  assert.equal(getParticleCount(animationConfig.minAnimatedViewportWidth), animationConfig.desktopParticleCount);
  assert.equal(getParticleCount(animationConfig.wideViewportWidth - 1), animationConfig.desktopParticleCount);
  assert.equal(getParticleCount(animationConfig.wideViewportWidth), animationConfig.wideParticleCount);
  assert.equal(getParticleCount(Number.NaN), 0);

  const particles = initParticles(1920, 1080, getFormBounds(1920, 1080), 10_000);
  assert.equal(particles.length, animationConfig.wideParticleCount);
});

test('disables animation for mobile, reduced motion, and hidden documents', () => {
  const desktopWidth = animationConfig.minAnimatedViewportWidth;

  assert.equal(shouldAnimateCanvas(desktopWidth, false, 'visible'), true);
  assert.equal(shouldAnimateCanvas(desktopWidth - 1, false, 'visible'), false);
  assert.equal(shouldAnimateCanvas(desktopWidth, true, 'visible'), false);
  assert.equal(shouldAnimateCanvas(desktopWidth, false, 'hidden'), false);
});

test('caps animation cadence at 24 FPS and drops catch-up work', () => {
  assert.equal(animationConfig.targetFps, 24);
  assert.equal(animationConfig.maxStepsPerFrame, 1);
  assert.equal(animationConfig.maxCatchUpMs, animationConfig.frameIntervalMs);

  let accumulatorMs = 0;
  let stepCount = 0;
  for (let frame = 0; frame < 60; frame += 1) {
    const timing = advanceFrameTiming(accumulatorMs, 1000 / 60);
    accumulatorMs = timing.accumulatorMs;
    if (timing.shouldStep) stepCount += 1;
  }
  assert.ok(stepCount > 0);
  assert.ok(stepCount <= animationConfig.targetFps);

  const delayedFrame = advanceFrameTiming(0, 5_000);
  assert.equal(delayedFrame.shouldStep, true);
  assert.equal(delayedFrame.clampedDeltaMs, animationConfig.maxCatchUpMs);
  assert.equal(delayedFrame.accumulatorMs, 0);

  const nextFrame = advanceFrameTiming(delayedFrame.accumulatorMs, 0);
  assert.equal(nextFrame.shouldStep, false);
});

test('bounds spatial candidate scans and connections independently of particle input size', () => {
  const particles = createDenseParticles(1_024);
  const selection = selectParticleConnections(particles, boundsOutsideCanvas);

  assert.equal(selection.candidateChecks, animationConfig.maxCandidateChecksPerFrame);
  assert.ok(selection.pairs.length > 0);
  assert.ok(selection.pairs.length <= animationConfig.maxConnectionsPerFrame);

  const degrees = new Uint16Array(particles.length);
  const uniquePairs = new Set();
  for (const [from, to] of selection.pairs) {
    assert.ok(from < to);
    degrees[from] += 1;
    degrees[to] += 1;
    uniquePairs.add(`${from}:${to}`);
  }

  assert.equal(uniquePairs.size, selection.pairs.length);
  assert.ok(degrees.every((degree) => degree <= animationConfig.maxConnectionsPerParticle));
});

test('caps all particle and pointer connection draws in one render', () => {
  let strokeCount = 0;
  const context = {
    beginPath() {},
    clearRect() {},
    fill() {},
    lineTo() {},
    moveTo() {},
    rect() {},
    stroke() {
      strokeCount += 1;
    },
  };

  renderParticles(context, 1920, 1080, createDenseParticles(1_024), { x: 20, y: 20, max: 20_000 }, boundsOutsideCanvas);

  assert.ok(strokeCount > 0);
  assert.ok(strokeCount <= animationConfig.maxConnectionsPerFrame + animationConfig.maxMouseConnectionsPerFrame);
});
