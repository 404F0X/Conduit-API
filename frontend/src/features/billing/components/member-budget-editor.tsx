import { useState } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useNavigate } from '@tanstack/react-router';
import { graphqlRequest } from '@/gql/graphql';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Textarea } from '@/components/ui/textarea';
import { proposeCommand, useGovernance } from '@/features/change-sets/data/governance';

type Budget = { limit: string | null; spent: string; reserved: string; remaining: string | null };
function databaseID(value: string) {
  const part = value.startsWith('gid://conduit/') ? value.split('/').at(-1)! : value;
  if (!/^\d+$/.test(part) || !Number.isSafeInteger(Number(part))) throw new Error('Invalid Project or member ID');
  return Number(part);
}
export function MemberBudgetEditor({ projectID, userID }: { projectID: string; userID: string }) {
  const { t } = useTranslation();
  const query = useQuery({
    queryKey: ['billing', 'member-budget', projectID, userID],
    enabled: !!projectID && !!userID,
    queryFn: () =>
      graphqlRequest<{ projectMemberSpendLimit: Budget }>(
        'query MemberBudget($projectID: ID!, $userID: ID!) { projectMemberSpendLimit(projectID: $projectID, userID: $userID) { limit spent reserved remaining } }',
        { projectID, userID }
      ).then((d) => d.projectMemberSpendLimit),
  });
  return (
    <details className='rounded-lg border p-4'>
      <summary className='min-h-10 cursor-pointer py-2 text-sm font-medium'>
        {t('billing.memberBudget.title', { defaultValue: 'Cumulative member budget across API keys' })}
      </summary>
      {query.isLoading ? (
        <p role='status'>{t('common.loading')}</p>
      ) : query.error ? (
        <p role='alert'>{query.error.message}</p>
      ) : (
        query.data && (
          <BudgetForm key={`${projectID}:${userID}:${query.data.limit}`} projectID={projectID} userID={userID} budget={query.data} />
        )
      )}
    </details>
  );
}
function BudgetForm({ projectID, userID, budget }: { projectID: string; userID: string; budget: Budget }) {
  const { t } = useTranslation();
  const policy = useGovernance();
  const client = useQueryClient();
  const navigate = useNavigate();
  const [unlimited, setUnlimited] = useState(budget.limit === null);
  const [limit, setLimit] = useState(budget.limit ?? '0');
  const [reason, setReason] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [operationID] = useState(() => crypto.randomUUID());
  const governed = policy.data?.governanceSettings.enabled && policy.data.governanceSettings.reviewKinds.includes('PROJECT_CONFIGURATION');
  async function save() {
    setBusy(true);
    setError('');
    try {
      if (governed) {
        const detail = await proposeCommand(
          {
            action: 'project.member_limit',
            targetIDs: [projectID],
            payload: { project_id: databaseID(projectID), user_id: databaseID(userID), limit: unlimited ? null : limit },
          },
          reason,
          operationID
        );
        await navigate({ to: '/change-sets/$changeSetId', params: { changeSetId: detail.summary.id } });
      } else {
        await graphqlRequest(
          'mutation MemberBudget($projectID: ID!, $userID: ID!, $limit: String) { setProjectMemberSpendLimit(projectID: $projectID, userID: $userID, limit: $limit) { limit spent reserved remaining } }',
          { projectID, userID, limit: unlimited ? null : limit }
        );
        await client.invalidateQueries({ queryKey: ['billing'] });
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className='space-y-3 pt-3'>
      <p className='text-muted-foreground text-sm'>
        {t('billing.memberBudget.explanation', {
          defaultValue:
            'Credits admission budget, separate from token and accounting-cost thresholds. Zero blocks new paid reservations; unlimited removes the member cap. Settled usage and history are retained.',
        })}
      </p>
      <p className='text-sm tabular-nums'>
        {t('billing.memberBudget.current', {
          defaultValue: 'Spent: {{spent}} · reserved: {{reserved}} · remaining: {{remaining}}',
          spent: budget.spent,
          reserved: budget.reserved,
          remaining: budget.remaining ?? t('billing.memberBudget.unlimited', { defaultValue: 'Unlimited' }),
        })}
      </p>
      <Label className='flex min-h-10 items-center gap-2'>
        <input type='checkbox' checked={unlimited} onChange={(e) => setUnlimited(e.target.checked)} disabled={busy} />
        {t('billing.memberBudget.unlimited', { defaultValue: 'Unlimited' })}
      </Label>
      {!unlimited && (
        <Input
          value={limit}
          onChange={(e) => setLimit(e.target.value)}
          inputMode='decimal'
          aria-label={t('billing.memberBudget.limit', { defaultValue: 'Credits limit' })}
          disabled={busy}
        />
      )}
      {governed && (
        <Textarea
          value={reason}
          onChange={(e) => setReason(e.target.value)}
          aria-label={t('governance.reason', { defaultValue: 'Reason for this change' })}
          placeholder={t('governance.reason', { defaultValue: 'Reason for this change' })}
          disabled={busy}
        />
      )}
      {error && (
        <p role='alert' className='text-destructive text-sm'>
          {error}
        </p>
      )}
      <Button
        onClick={() => void save()}
        disabled={
          busy || policy.isPending || policy.isError || (!unlimited && !/^\d+(?:\.\d{1,6})?$/.test(limit)) || (governed && !reason.trim())
        }
      >
        {t(governed ? 'governance.propose' : 'common.save', { defaultValue: governed ? 'Create proposal' : 'Save' })}
      </Button>
    </div>
  );
}
