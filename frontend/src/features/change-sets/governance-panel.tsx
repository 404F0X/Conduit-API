import { useState } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { useNavigate } from '@tanstack/react-router';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { usePermissions } from '@/hooks/usePermissions';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Textarea } from '@/components/ui/textarea';
import { CHANGE_SET_KINDS, type ChangeSetKind } from './data/change-sets';
import { proposeCommand, updateGovernance, useGovernance, type GovernancePolicy } from './data/governance';

export function GovernancePanel() {
  const query = useGovernance();
  const { t } = useTranslation();
  const permissions = usePermissions();
  const [open, setOpen] = useState(false);
  return (
    <details open={open} onToggle={(e) => setOpen(e.currentTarget.open)} className='bg-card rounded-xl border p-4'>
      <summary className='min-h-10 cursor-pointer py-2 font-medium'>
        {t('governance.title', { defaultValue: 'Approval policy and proposals' })}
      </summary>
      {open &&
        (query.isLoading ? (
          <p role='status'>{t('common.loading')}</p>
        ) : query.isError ? (
          <p role='alert'>{query.error.message}</p>
        ) : (
          query.data && (
            <div className='space-y-5 pt-4'>
              <p className='text-muted-foreground text-sm'>
                {t('governance.explanation', {
                  defaultValue:
                    'Approval is optional in both modes. Enabling it changes future writes; existing pending packages keep their state.',
                })}
              </p>
              {permissions.hasSystemScope('manage_governance') && (
                <PolicyEditor key={query.data.governanceSettings.revision} policy={query.data.governanceSettings} />
              )}
              <CommandEditor
                actions={query.data.governanceCapabilities.actions.filter(
                  (a) => a.supported && (permissions.hasSystemScope(a.domainScope) || permissions.hasScope(a.domainScope))
                )}
              />
            </div>
          )
        ))}
    </details>
  );
}
function PolicyEditor({ policy }: { policy: GovernancePolicy }) {
  const { t } = useTranslation();
  const client = useQueryClient();
  const navigate = useNavigate();
  const [enabled, setEnabled] = useState(policy.enabled);
  const [kinds, setKinds] = useState(policy.reviewKinds);
  const [choice, setChoice] = useState(policy.enabled ? (policy.allowSelfReview ? 'self' : 'strict') : '');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [reason, setReason] = useState('');
  const [operationID] = useState(() => crypto.randomUUID());
  const reduction =
    policy.enabled &&
    policy.reviewKinds.includes('SYSTEM_CONFIGURATION') &&
    (!enabled || (choice === 'self' && !policy.allowSelfReview) || policy.reviewKinds.some((k) => !kinds.includes(k)));
  async function save() {
    setBusy(true);
    setError('');
    try {
      if (reduction) {
        const saved = await proposeCommand(
          {
            action: 'system.setting',
            targetIDs: [],
            payload: {
              key: 'system_governance_settings',
              value: {
                revision: Number(policy.revision) + 1,
                enabled,
                reviewKinds: kinds.map((k) => k.toLowerCase()),
                allowSelfReview: choice === 'self',
                legacyReviewScopes: false,
              },
            },
          },
          reason,
          operationID
        );
        await navigate({ to: '/change-sets/$changeSetId', params: { changeSetId: saved.summary.id } });
      } else {
        await updateGovernance({ enabled, reviewKinds: kinds, allowSelfReview: choice ? choice === 'self' : undefined }, policy.revision);
      }
      await client.invalidateQueries({ queryKey: ['governance'] });
      toast.success(t('common.saveSuccess', { defaultValue: 'Saved' }));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <section className='space-y-3'>
      <Label className='flex min-h-10 items-center gap-3'>
        <input type='checkbox' checked={enabled} onChange={(e) => setEnabled(e.target.checked)} disabled={busy} />
        {t('governance.enabled', { defaultValue: 'Require approval for selected domains' })}
      </Label>
      <div className='grid gap-2 sm:grid-cols-2 lg:grid-cols-3'>
        {CHANGE_SET_KINDS.map((kind) => (
          <Label key={kind} className='flex min-h-10 items-center gap-2'>
            <input
              type='checkbox'
              checked={kinds.includes(kind)}
              onChange={(e) => setKinds(e.target.checked ? [...kinds, kind] : kinds.filter((k) => k !== kind))}
              disabled={busy}
            />
            {t(`eventPackages.${kind}`)}
          </Label>
        ))}
      </div>
      <fieldset className='space-y-2 rounded-lg border p-3'>
        <legend className='px-1 text-sm'>{t('governance.reviewChoice', { defaultValue: 'Choose who may review' })}</legend>
        {(['self', 'strict'] as const).map((value) => (
          <Label key={value} className='flex min-h-10 items-center gap-2'>
            <input
              name='governance-review-choice'
              type='radio'
              checked={choice === value}
              onChange={() => setChoice(value)}
              disabled={busy}
            />
            {t(`governance.${value}`, {
              defaultValue:
                value === 'self' ? 'Allow an authorized administrator to review their own proposal' : 'Require another qualified reviewer',
            })}
          </Label>
        ))}
      </fieldset>
      {reduction && (
        <Textarea
          value={reason}
          onChange={(e) => setReason(e.target.value)}
          placeholder={t('governance.reason', { defaultValue: 'Reason for this change' })}
          aria-label={t('governance.reason', { defaultValue: 'Reason for this change' })}
        />
      )}
      {error && (
        <p role='alert' className='text-destructive text-sm'>
          {error}
        </p>
      )}
      <Button disabled={busy || (enabled && !choice) || (reduction && !reason.trim())} onClick={() => void save()}>
        {t(reduction ? 'governance.propose' : 'common.save', { defaultValue: reduction ? 'Create proposal' : 'Save' })}
      </Button>
    </section>
  );
}
function CommandEditor({ actions }: { actions: Array<{ action: string; kind: ChangeSetKind; domainScope: string }> }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [action, setAction] = useState(actions[0]?.action ?? '');
  const [targets, setTargets] = useState('');
  const [payload, setPayload] = useState('{}');
  const [reason, setReason] = useState('');
  const [operationID] = useState(() => crypto.randomUUID());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  if (!actions.length) return null;
  async function submit() {
    setBusy(true);
    setError('');
    try {
      const value: unknown = JSON.parse(payload);
      if (!value || typeof value !== 'object' || Array.isArray(value))
        throw new Error(t('governance.objectRequired', { defaultValue: 'Enter the command fields as a JSON object.' }));
      const detail = await proposeCommand(
        { action, targetIDs: targets.split(/[\s,]+/).filter(Boolean), payload: value as Record<string, unknown> },
        reason,
        operationID
      );
      setPayload('{}');
      await navigate({ to: '/change-sets/$changeSetId', params: { changeSetId: detail.summary.id } });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <details className='rounded-lg border p-3'>
      <summary className='min-h-10 cursor-pointer py-2 text-sm'>
        {t('governance.advancedProposal', { defaultValue: 'Advanced management proposal' })}
      </summary>
      <div className='space-y-3 pt-3'>
        <Label htmlFor='governed-action'>{t('governance.action', { defaultValue: 'Action' })}</Label>
        <select
          id='governed-action'
          className='bg-background min-h-10 w-full rounded-md border px-3'
          value={action}
          onChange={(e) => setAction(e.target.value)}
          disabled={busy}
        >
          {actions.map((a) => (
            <option key={a.action} value={a.action}>
              {a.action}
            </option>
          ))}
        </select>
        <Label htmlFor='governed-targets'>{t('governance.targets', { defaultValue: 'Target IDs (leave empty to create)' })}</Label>
        <Input id='governed-targets' value={targets} onChange={(e) => setTargets(e.target.value)} disabled={busy} />
        <Label htmlFor='governed-payload'>{t('governance.fields', { defaultValue: 'Command fields' })}</Label>
        <Textarea
          id='governed-payload'
          className='min-h-36 font-mono'
          value={payload}
          onChange={(e) => setPayload(e.target.value)}
          disabled={busy}
          autoComplete='off'
          spellCheck={false}
        />
        <Label htmlFor='governed-reason'>{t('governance.reason', { defaultValue: 'Reason for this change' })}</Label>
        <Textarea id='governed-reason' value={reason} onChange={(e) => setReason(e.target.value)} disabled={busy} />
        {error && (
          <p role='alert' className='text-destructive text-sm'>
            {error}
          </p>
        )}
        <Button onClick={() => void submit()} disabled={busy || !reason.trim() || !action}>
          {t('governance.propose', { defaultValue: 'Create proposal' })}
        </Button>
      </div>
    </details>
  );
}
