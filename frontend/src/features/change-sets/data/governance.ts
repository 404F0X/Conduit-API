import { useQuery } from '@tanstack/react-query';
import { graphqlRequest } from '@/gql/graphql';
import { DETAIL_FIELDS, type ChangeSetKind, type PackageDetail } from './change-sets';

export type GovernancePolicy = {
  revision: string;
  enabled: boolean;
  reviewKinds: ChangeSetKind[];
  allowSelfReview: boolean;
  legacyReviewScopes: boolean;
};
export type GovernanceCommand = { action: string; targetIDs: string[]; payload: Record<string, unknown> };
export type GovernanceCapabilities = {
  actions: Array<{ action: string; kind: ChangeSetKind; domainScope: string; supported: boolean }>;
  eligibleReviewerCount: number;
};
const POLICY_FIELDS = 'revision enabled reviewKinds allowSelfReview legacyReviewScopes';
export function useGovernance() {
  return useQuery({
    queryKey: ['governance'],
    queryFn: () =>
      graphqlRequest<{ governanceSettings: GovernancePolicy; governanceCapabilities: GovernanceCapabilities }>(
        `query Governance { governanceSettings { ${POLICY_FIELDS} } governanceCapabilities { actions { action kind domainScope supported } eligibleReviewerCount } }`
      ),
  });
}
export function updateGovernance(
  input: { enabled: boolean; reviewKinds: ChangeSetKind[]; allowSelfReview?: boolean },
  expectedRevision: string
) {
  return graphqlRequest<{ updateGovernanceSettings: GovernancePolicy }>(
    `mutation GovernancePolicy($input: UpdateGovernanceSettingsInput!, $expectedRevision: String!) { updateGovernanceSettings(input: $input, expectedRevision: $expectedRevision) { ${POLICY_FIELDS} } }`,
    { input, expectedRevision }
  );
}
// Keep only the fingerprint and revision in memory. Retrying a lost response
// must use the original revision even if publication changed the live target.
const proposalIntents = new Map<string, { fingerprint: string; operationID: string; revision?: string }>();
export async function proposeCommand(command: GovernanceCommand, reason: string, intentID: string) {
  const bytes = new TextEncoder().encode(JSON.stringify({ command, reason: reason.trim() }));
  const fingerprint = Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', bytes)), (v) => v.toString(16).padStart(2, '0')).join(
    ''
  );
  let intent = proposalIntents.get(intentID);
  if (!intent || intent.fingerprint !== fingerprint) {
    intent = { fingerprint, operationID: intent ? crypto.randomUUID() : intentID };
    if (proposalIntents.size >= 100) proposalIntents.delete(proposalIntents.keys().next().value!);
    proposalIntents.set(intentID, intent);
  }
  if (!intent.revision) {
    const base = await graphqlRequest<{ governedChangeBase: string }>(
      `query GovernedBase($action: String!, $targetIDs: [ID!]!, $payload: JSON!) { governedChangeBase(action: $action, targetIDs: $targetIDs, payload: $payload) }`,
      command
    );
    intent.revision = base.governedChangeBase;
  }
  return graphqlRequest<{ proposeGovernedChange: PackageDetail }>(
    `mutation GovernedProposal($input: ProposeGovernedChangeInput!) { proposeGovernedChange(input: $input) { ${DETAIL_FIELDS} } }`,
    { input: { ...command, reason, operationID: intent.operationID, expectedTargetRevision: intent.revision } }
  ).then((d) => d.proposeGovernedChange);
}
export function claimGovernedSecrets(id: string) {
  return graphqlRequest<{ claimGovernedChangeSecrets: unknown }>(
    'mutation ClaimGovernedSecrets($id: ID!) { claimGovernedChangeSecrets(id: $id) }',
    { id }
  ).then((d) => d.claimGovernedChangeSecrets);
}
