import { expect, type Page } from '@playwright/test';
import { gotoAndEnsureAuth } from './auth.utils';

const fixtureProjectName = 'Default';

// The isolated setup owner already belongs to Default. Resolve its real ID
// from authorized Projects and use the same switcher as a user with several Projects.
export async function enterFixtureProject(page: Page, path: string) {
  await gotoAndEnsureAuth(page, '/');
  const data = await page.evaluate(async () => {
    const token = localStorage.getItem('conduit_access_token');
    if (!token) throw new Error('Fixture requires an authenticated session');
    const response = await fetch('/admin/graphql', {
      method: 'POST',
      headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
      body: JSON.stringify({ query: 'query E2EEntryProjects { myProjects { id name } productExperienceSettings { mode revision } }' }),
    });
    const payload = await response.json();
    if (!response.ok || payload.errors) throw new Error('Unable to read authorized fixture Projects');
    return payload.data as {
      myProjects: Array<{ id: string; name: string }>;
      productExperienceSettings: { mode: string; revision: string };
    };
  });
  expect(['SIMPLE', 'ENTERPRISE']).toContain(data.productExperienceSettings.mode);
  if (data.productExperienceSettings.mode === 'SIMPLE') {
    const updated = await page.evaluate(async (expectedRevision) => {
      const token = localStorage.getItem('conduit_access_token');
      const response = await fetch('/admin/graphql', {
        method: 'POST',
        headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
        body: JSON.stringify({
          query: `mutation E2EEnterpriseMode($input: UpdateProductExperienceSettingsInput!) {
          updateProductExperienceSettings(input: $input) { mode revision }
        }`,
          variables: { input: { mode: 'ENTERPRISE', expectedRevision } },
        }),
      });
      return { status: response.status, payload: await response.json() };
    }, data.productExperienceSettings.revision);
    expect(updated.status).toBe(200);
    expect(updated.payload.errors).toBeUndefined();
    expect(updated.payload.data.updateProductExperienceSettings.mode).toBe('ENTERPRISE');
  }
  // A normal navigation loads the acknowledged server configuration for this
  // isolated fixture; no mode override or product default is changed in code.
  await page.goto(path);
  const project = data.myProjects.find((candidate) => candidate.name === fixtureProjectName);
  if (!project) throw new Error('Isolated setup owner must have access to the Default fixture Project');
  const switcher = page.locator('header button').filter({ has: page.locator('svg.lucide-chevrons-up-down') });
  await expect(switcher).toHaveCount(1);
  await switcher.click();
  // The selected item includes a checkmark in its accessible name.
  await page
    .getByRole('menuitem')
    .filter({ has: page.getByText(fixtureProjectName, { exact: true }) })
    .click();
  await expect(switcher).toHaveText(fixtureProjectName);
  await expect.poll(() => page.evaluate(() => localStorage.getItem('conduit_selected_project_id'))).toBe(project.id);
}
