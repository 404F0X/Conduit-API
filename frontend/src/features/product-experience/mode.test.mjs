import assert from 'node:assert/strict';
import test from 'node:test';
import { isProductModeAllowed, resolveProductLandingPath, resolveProjectSelection } from './mode.ts';

test('landing matrix keeps owners on admin home and users in their project', () => {
  for (const mode of ['SIMPLE', 'ENTERPRISE']) {
    assert.equal(resolveProductLandingPath(mode, true), '/');
    assert.equal(resolveProductLandingPath(mode, false), '/project/dashboard');
  }
});

test('enterprise-only routes are hidden in simple mode', () => {
  assert.equal(isProductModeAllowed('SIMPLE', ['ENTERPRISE']), false);
  assert.equal(isProductModeAllowed('ENTERPRISE', ['ENTERPRISE']), true);
  assert.equal(isProductModeAllowed('SIMPLE'), true);
  assert.equal(isProductModeAllowed('ENTERPRISE'), true);
});

test('simple mode selects only the resolved primary project', () => {
  const projects = ['project-1', 'project-2'];
  assert.equal(resolveProjectSelection('SIMPLE', 'project-2', projects, 'project-1'), 'project-1');
  assert.equal(resolveProjectSelection('SIMPLE', 'project-2', projects, null), null);
  assert.equal(resolveProjectSelection('SIMPLE', 'project-2', projects, 'project-3'), null);
});

test('enterprise mode preserves a valid selection and otherwise chooses the first project', () => {
  const projects = ['project-1', 'project-2'];
  assert.equal(resolveProjectSelection('ENTERPRISE', 'project-2', projects, null), 'project-2');
  assert.equal(resolveProjectSelection('ENTERPRISE', 'missing', projects, null), 'project-1');
  assert.equal(resolveProjectSelection('ENTERPRISE', null, [], null), null);
});
