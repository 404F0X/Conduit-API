import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';

const sourceRoot = join(import.meta.dirname, '..', '..');
const readSource = (...parts) => readFileSync(join(sourceRoot, ...parts), 'utf8');

test('redemption list requests masked hints while creation receives one-time plaintext codes', () => {
  const source = readSource('features', 'billing', 'redemption-data.ts');
  const adminSource = readSource('features', 'billing', 'components', 'redemption-code-section.tsx');
  const listFields = source.slice(source.indexOf('const CODE_FIELDS'), source.indexOf('const REDEMPTION_CODES_QUERY'));
  const createMutation = source.slice(
    source.indexOf('const CREATE_REDEMPTION_CODES_MUTATION'),
    source.indexOf('const REVOKE_REDEMPTION_CODE_MUTATION')
  );

  assert.match(listFields, /codeHint/);
  assert.match(listFields, /maxRedemptions redemptionCount remainingRedemptions/);
  assert.doesNotMatch(listFields, /\bcode\b/);
  assert.match(createMutation, /codes \{ id code codeHint \}/);
  assert.match(createMutation, /maxRedemptions/);
  assert.match(adminSource, /const \[maxRedemptions, setMaxRedemptions\] = useState\(1\)/);
  assert.match(adminSource, /max=\{100_000\}/);
  assert.match(adminSource, /code\.redemptionCount[\s\S]*code\.maxRedemptions/);
});

test('wallet redemption remains Project-bound and refreshes the wallet surface', () => {
  const dataSource = readSource('features', 'billing', 'redemption-data.ts');
  const walletSource = readSource('features', 'wallet', 'index.tsx');
  const dialogSource = readSource('features', 'wallet', 'components', 'redeem-code-dialog.tsx');

  assert.match(dataSource, /\{ 'X-Project-ID': selectedProjectID \}/);
  assert.match(dataSource, /my-project-balance/);
  assert.match(dataSource, /Promise\.allSettled/);
  assert.match(dataSource, /onError:\s*\(\)\s*=>\s*undefined/);
  assert.match(walletSource, /<RedeemCodeDialog/);
  assert.match(dialogSource, /wallet\.redeem\.errors\.generic/);
  assert.match(dialogSource, /try\s*\{[\s\S]*await redeem\.mutateAsync\(normalizedCode\)[\s\S]*\}\s*catch\s*\(error\)\s*\{/);
  assert.match(dialogSource, /if \(!isAuthError\(error\)\)\s*\{[\s\S]*wallet\.redeem\.errors\.generic/);
  assert.doesNotMatch(dialogSource, /error\.message|REDEMPTION_CODE_(?:EXPIRED|REVOKED|ALREADY_REDEEMED)/);
});

test('admin redemption mutations delegate exactly one error toast to their components', () => {
  const dataSource = readSource('features', 'billing', 'redemption-data.ts');
  const adminSource = readSource('features', 'billing', 'components', 'redemption-code-section.tsx');
  const createHook = dataSource.slice(
    dataSource.indexOf('export function useCreateCreditRedemptionCodes'),
    dataSource.indexOf('export function useRevokeCreditRedemptionCode')
  );
  const revokeHook = dataSource.slice(
    dataSource.indexOf('export function useRevokeCreditRedemptionCode'),
    dataSource.indexOf('export function useRedeemCreditCode')
  );
  const revokeHandler = adminSource.slice(
    adminSource.indexOf('const revokeCode = async'),
    adminSource.indexOf('\n  return (', adminSource.indexOf('const revokeCode = async'))
  );
  const createHandler = adminSource.slice(adminSource.indexOf('const submit = async'), adminSource.indexOf('const copyCodes = async'));

  assert.match(createHook, /onError:\s*\(\)\s*=>\s*undefined/);
  assert.match(revokeHook, /onError:\s*\(\)\s*=>\s*undefined/);
  assert.match(revokeHandler, /catch\s*\(error\)\s*\{[\s\S]*if \(!isAuthError\(error\)\)[\s\S]*billing\.redemption\.admin\.revokeError/);
  assert.match(createHandler, /catch\s*\(error\)\s*\{[\s\S]*if \(!isAuthError\(error\)\)[\s\S]*billing\.redemption\.admin\.createError/);
});

test('billing route guard matches the any-of scopes used by navigation', () => {
  const routeSource = readSource('routes', '_authenticated', 'billing', 'index.tsx');

  assert.match(routeSource, /requiredScopes=\{\['read_billing', 'read_subscriptions'\]\}/);
  assert.doesNotMatch(routeSource, /requireAll/);
});
