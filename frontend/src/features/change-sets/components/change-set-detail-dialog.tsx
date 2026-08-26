import { AlertTriangle, Clock3, FileDiff, Info } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { JsonViewer } from '@/components/json-tree-view';
import { formatChangeSetTime, hasJsonContent } from '../change-set-display';
import type { ChangeSet, ChangeSetItem } from '../data/change-sets';
import { ChangeSetActions, type ChangeSetActionType } from './change-set-actions';
import { ChangeSetKindBadge, ChangeSetStatusBadge } from './change-set-badges';

type Props = {
  changeSet: ChangeSet | null;
  canManage: boolean;
  onOpenChange: (open: boolean) => void;
  onAction: (action: ChangeSetActionType, changeSet: ChangeSet) => void;
};

function DetailField({ label, children, mono = false }: { label: string; children: React.ReactNode; mono?: boolean }) {
  return (
    <div className='min-w-0'>
      <dt className='text-muted-foreground text-xs font-medium'>{label}</dt>
      <dd className={`mt-1 text-sm break-words ${mono ? 'font-mono' : ''}`}>{children || '-'}</dd>
    </div>
  );
}

function Snapshot({ label, value }: { label: string; value: unknown }) {
  const { t } = useTranslation();

  return (
    <section className='border-border min-w-0 overflow-hidden rounded-md border'>
      <h4 className='bg-muted/40 border-border border-b px-3 py-2 text-xs font-semibold'>{label}</h4>
      <div className='max-h-80 min-h-28 overflow-auto p-3'>
        {hasJsonContent(value) ? (
          <JsonViewer data={value} rootName='' expandDepth={2} className='text-xs' />
        ) : (
          <div className='text-muted-foreground flex min-h-20 items-center justify-center text-xs'>{t('changeSets.detail.noSnapshot')}</div>
        )}
      </div>
    </section>
  );
}

function ChangeItemView({ item, index }: { item: ChangeSetItem; index: number }) {
  const { t } = useTranslation();

  return (
    <details className='border-border group border-b last:border-b-0' open={index === 0}>
      <summary className='hover:bg-muted/35 focus-visible:bg-muted/35 flex cursor-pointer list-none items-center justify-between gap-3 px-4 py-3 focus-visible:outline-none'>
        <div className='min-w-0'>
          <div className='flex min-w-0 flex-wrap items-center gap-2'>
            <span className='bg-muted rounded-sm px-1.5 py-0.5 font-mono text-xs font-semibold'>
              {t(`changeSets.action.${item.action}`)}
            </span>
            <span className='min-w-0 font-mono text-sm break-all'>{item.itemKey}</span>
          </div>
          {item.validationError && <p className='text-destructive mt-1 text-xs break-words'>{item.validationError}</p>}
        </div>
        <FileDiff className='text-muted-foreground size-4 shrink-0' />
      </summary>
      <div className='grid gap-3 px-4 pb-4 lg:grid-cols-3'>
        <Snapshot label={t('changeSets.detail.before')} value={item.beforeSnapshot} />
        <Snapshot label={t('changeSets.detail.after')} value={item.afterSnapshot} />
        <Snapshot label={t('changeSets.detail.source')} value={item.sourceSnapshot} />
      </div>
    </details>
  );
}

export function ChangeSetDetailDialog({ changeSet, canManage, onOpenChange, onAction }: Props) {
  const { t, i18n } = useTranslation();

  if (!changeSet) return null;
  const canAct = canManage && ['DRAFT', 'PENDING_REVIEW', 'INVALID'].includes(changeSet.status);

  return (
    <Dialog open onOpenChange={onOpenChange}>
      <DialogContent className='max-h-[calc(100svh-1rem)] grid-rows-[auto_minmax(0,1fr)_auto] gap-0 overflow-hidden rounded-md p-0 sm:max-w-6xl'>
        <DialogHeader className='border-border border-b px-4 py-4 pr-12 text-left sm:px-5'>
          <div className='flex min-w-0 flex-wrap items-center gap-2'>
            <ChangeSetStatusBadge status={changeSet.status} />
            <ChangeSetKindBadge kind={changeSet.kind} />
          </div>
          <DialogTitle className='mt-1 text-base break-words sm:text-lg'>{changeSet.title}</DialogTitle>
          <DialogDescription className='truncate font-mono text-xs'>{changeSet.id}</DialogDescription>
        </DialogHeader>

        <Tabs defaultValue='overview' className='min-h-0 gap-0 overflow-hidden'>
          <div className='border-border border-b px-4 py-2 sm:px-5'>
            <TabsList className='h-8 max-w-full rounded-md'>
              <TabsTrigger value='overview' className='rounded-sm px-3 text-xs sm:text-sm'>
                <Info className='size-4' />
                {t('changeSets.detail.overview')}
              </TabsTrigger>
              <TabsTrigger value='changes' className='rounded-sm px-3 text-xs sm:text-sm'>
                <FileDiff className='size-4' />
                {t('changeSets.detail.changes', { count: changeSet.items.length })}
              </TabsTrigger>
              <TabsTrigger value='timeline' className='rounded-sm px-3 text-xs sm:text-sm'>
                <Clock3 className='size-4' />
                {t('changeSets.detail.timeline', { count: changeSet.events.length })}
              </TabsTrigger>
            </TabsList>
          </div>

          <TabsContent value='overview' className='min-h-0 overflow-auto p-4 sm:p-5'>
            <div className='space-y-6'>
              {(changeSet.validationError || changeSet.reviewNote) && (
                <div className='space-y-3'>
                  {changeSet.validationError && (
                    <div className='border-destructive/40 bg-destructive/5 flex gap-3 rounded-md border p-3'>
                      <AlertTriangle className='text-destructive mt-0.5 size-4 shrink-0' />
                      <div className='min-w-0'>
                        <p className='text-sm font-medium'>{t('changeSets.detail.validationError')}</p>
                        <p className='text-destructive mt-1 text-xs break-words'>{changeSet.validationError}</p>
                      </div>
                    </div>
                  )}
                  {changeSet.reviewNote && (
                    <div className='border-border bg-muted/25 rounded-md border p-3'>
                      <p className='text-sm font-medium'>{t('changeSets.detail.reviewNote')}</p>
                      <p className='text-muted-foreground mt-1 text-sm break-words whitespace-pre-wrap'>{changeSet.reviewNote}</p>
                    </div>
                  )}
                </div>
              )}

              <section>
                <h3 className='border-border border-b pb-2 text-sm font-semibold'>{t('changeSets.detail.identity')}</h3>
                <dl className='mt-3 grid gap-x-6 gap-y-4 sm:grid-cols-2 lg:grid-cols-3'>
                  <DetailField label={t('changeSets.columns.kind')}>{t(`changeSets.kind.${changeSet.kind}`)}</DetailField>
                  <DetailField label={t('changeSets.columns.status')}>{t(`changeSets.status.${changeSet.status}`)}</DetailField>
                  <DetailField label={t('changeSets.columns.items')}>{changeSet.items.length}</DetailField>
                  <DetailField label={t('changeSets.detail.scopeType')} mono>
                    {changeSet.scopeType}
                  </DetailField>
                  <DetailField label={t('changeSets.detail.scopeID')} mono>
                    {changeSet.scopeID}
                  </DetailField>
                  <DetailField label={t('changeSets.detail.target')} mono>
                    {changeSet.appliedTargetType && changeSet.appliedTargetID
                      ? `${changeSet.appliedTargetType} / ${changeSet.appliedTargetID}`
                      : '-'}
                  </DetailField>
                </dl>
              </section>

              <section>
                <h3 className='border-border border-b pb-2 text-sm font-semibold'>{t('changeSets.detail.lifecycle')}</h3>
                <dl className='mt-3 grid gap-x-6 gap-y-4 sm:grid-cols-2 lg:grid-cols-3'>
                  <DetailField label={t('changeSets.detail.created')}>
                    {formatChangeSetTime(changeSet.createdAt, i18n.language)}
                    {changeSet.createdBy && (
                      <span className='text-muted-foreground mt-1 block font-mono text-xs'>
                        {t('changeSets.detail.actorID')} / {changeSet.createdBy}
                      </span>
                    )}
                  </DetailField>
                  <DetailField label={t('changeSets.detail.submitted')}>
                    {formatChangeSetTime(changeSet.submittedAt, i18n.language)}
                    {changeSet.submittedBy && (
                      <span className='text-muted-foreground mt-1 block font-mono text-xs'>
                        {t('changeSets.detail.actorID')} / {changeSet.submittedBy}
                      </span>
                    )}
                  </DetailField>
                  <DetailField label={t('changeSets.detail.reviewed')}>
                    {formatChangeSetTime(changeSet.reviewedAt, i18n.language)}
                    {changeSet.reviewedBy && (
                      <span className='text-muted-foreground mt-1 block font-mono text-xs'>
                        {t('changeSets.detail.actorID')} / {changeSet.reviewedBy}
                      </span>
                    )}
                  </DetailField>
                  <DetailField label={t('changeSets.detail.applied')}>
                    {formatChangeSetTime(changeSet.appliedAt, i18n.language)}
                  </DetailField>
                  <DetailField label={t('changeSets.detail.updated')}>
                    {formatChangeSetTime(changeSet.updatedAt, i18n.language)}
                  </DetailField>
                </dl>
              </section>

              <section>
                <h3 className='border-border border-b pb-2 text-sm font-semibold'>{t('changeSets.detail.revisions')}</h3>
                <dl className='mt-3 grid gap-x-6 gap-y-4 sm:grid-cols-2'>
                  <DetailField label={t('changeSets.detail.baseRevision')} mono>
                    {changeSet.baseRevision}
                  </DetailField>
                  <DetailField label={t('changeSets.detail.sourceRevision')} mono>
                    {changeSet.sourceRevision}
                  </DetailField>
                </dl>
              </section>
            </div>
          </TabsContent>

          <TabsContent value='changes' className='min-h-0 overflow-auto'>
            {changeSet.items.length > 0 ? (
              changeSet.items.map((item, index) => <ChangeItemView key={item.id} item={item} index={index} />)
            ) : (
              <div className='text-muted-foreground flex min-h-64 items-center justify-center p-6 text-sm'>
                {t('changeSets.detail.noChanges')}
              </div>
            )}
          </TabsContent>

          <TabsContent value='timeline' className='min-h-0 overflow-auto p-4 sm:p-5'>
            {changeSet.events.length > 0 ? (
              <ol className='relative ml-2 border-l'>
                {changeSet.events.map((event) => (
                  <li key={event.id} className='relative pb-6 pl-6 last:pb-0'>
                    <span className='bg-background border-primary absolute top-1 -left-[5px] size-2.5 rounded-full border-2' />
                    <div className='flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1'>
                      <p className='text-sm font-semibold break-words'>{event.eventType}</p>
                      <time className='text-muted-foreground text-xs'>{formatChangeSetTime(event.createdAt, i18n.language)}</time>
                    </div>
                    <p className='text-muted-foreground mt-1 font-mono text-xs'>
                      {event.actorType} / {event.actorID || '-'}
                    </p>
                    {hasJsonContent(event.detail) && (
                      <div className='bg-muted/25 border-border mt-3 overflow-hidden rounded-md border p-3'>
                        <JsonViewer data={event.detail} rootName='' expandDepth={1} className='text-xs' />
                      </div>
                    )}
                  </li>
                ))}
              </ol>
            ) : (
              <div className='text-muted-foreground flex min-h-64 items-center justify-center text-sm'>
                {t('changeSets.detail.noEvents')}
              </div>
            )}
          </TabsContent>
        </Tabs>

        {canAct && (
          <div className='border-border bg-background border-t px-4 py-3 sm:px-5'>
            <ChangeSetActions changeSet={changeSet} onAction={onAction} className='flex flex-wrap justify-end gap-2' />
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
