import { useMemo, useState } from 'react';
import { IconCopy, IconDots, IconPlus } from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { DEFAULT_CREDIT_DISPLAY_NAME } from '@/lib/accounting';
import { cn } from '@/lib/utils';
import { usePermissions } from '@/hooks/usePermissions';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import { CopyButton } from '@/components/ui/copy-button';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Input } from '@/components/ui/input';
import { Progress } from '@/components/ui/progress';
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from '@/components/ui/sheet';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import { ConfirmDialog } from '@/components/confirm-dialog';
import { Header } from '@/components/layout/header';
import { Main } from '@/components/layout/main';
import { EmptyState, ErrorState, LoadingRows, PageHeading } from '@/components/page-state';
import {
  useApiKeyQuotaUsages,
  useAssignableModelGroups,
  useApiKeyCreationCatalog,
  useApiKeyProfileCatalog,
  useCreateApiKey,
  useRotateApiKey,
  useUpdateApiKey,
  useUpdateApiKeyProfiles,
  useUpdateApiKeyStatus,
} from '@/features/apikeys/data/apikeys';
import { useGeneralSettings } from '@/features/system/data/system';
import {
  activeProfileOf,
  buildSimpleTokenCreation,
  planSimpleTokenUpdate,
  hasAdvancedSettings,
  type SimpleToken,
  type SimpleTokenForm,
  useSimpleTokens,
} from '../data/tokens';
import { formatFullTime, formatMoney, formatRelativeTime } from '../lib/format';
import { TokenFormDialog } from './token-form-dialog';
import { UsageExamples } from './usage-examples';

const PAGE_SIZE = 20;

function maskKey(key: string) {
  if (key.length <= 10) return '••••••';
  return `${key.slice(0, 5)}••••••${key.slice(-4)}`;
}

function toNumber(value: unknown): number | null {
  if (value === null || value === undefined || value === '') return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

function copyText(text: string, message: string) {
  navigator.clipboard
    .writeText(text)
    .then(() => toast.success(message))
    .catch(() => toast.error(message));
}

function formFromToken(token: SimpleToken): SimpleTokenForm {
  const profile = activeProfileOf(token);
  return {
    name: token.name,
    quota: toNumber(profile?.quota?.cost),
    expiresAt: profile?.validUntil ? new Date(profile.validUntil) : null,
    modelIds: profile?.modelIDs ?? [],
  };
}

/** 已用 / 额度：有金额额度时显示进度条，没有就写“不限”。 */
function TokenUsageCell({ token, creditLabel }: { token: SimpleToken; creditLabel: string }) {
  const { t } = useTranslation();
  const profile = activeProfileOf(token);
  const quota = toNumber(profile?.quota?.cost);
  const usages = useApiKeyQuotaUsages(token.id, { enabled: quota != null });
  if (quota == null) return <span className='text-muted-foreground text-sm'>{t('simple.tokens.unlimited')}</span>;
  const usage = usages.data?.find((item) => item.profileName === profile?.name) ?? usages.data?.[0];
  const used = toNumber(usage?.usage?.totalCost) ?? 0;
  const percent = Math.min(100, quota > 0 ? (used / quota) * 100 : 0);
  return (
    <div className='min-w-32 space-y-1'>
      <div className='text-sm tabular-nums'>
        {formatMoney(creditLabel, used, t).text} / {formatMoney(creditLabel, quota, t).text}
      </div>
      <Progress value={percent} className={cn('h-1.5', percent >= 90 && '[&>div]:bg-red-500')} />
    </div>
  );
}

function ModelsCell({ token }: { token: SimpleToken }) {
  const { t } = useTranslation();
  const ids = activeProfileOf(token)?.modelIDs ?? [];
  if (ids.length === 0) return <span className='text-sm'>{t('simple.tokens.allModels')}</span>;
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <span className='text-sm'>{t('simple.tokens.modelCount', { count: ids.length })}</span>
      </TooltipTrigger>
      <TooltipContent className='max-w-xs font-mono text-xs'>{ids.join(', ')}</TooltipContent>
    </Tooltip>
  );
}

function ExpiryCell({ token }: { token: SimpleToken }) {
  const { t, i18n } = useTranslation();
  const until = activeProfileOf(token)?.validUntil;
  if (!until) return <span className='text-muted-foreground text-sm'>{t('simple.tokens.neverExpires')}</span>;
  const date = new Date(until);
  const expired = date.getTime() < Date.now();
  return (
    <span className={cn('text-sm', expired && 'text-red-600')}>
      {expired ? t('simple.tokens.expired') : new Intl.DateTimeFormat(i18n.language, { dateStyle: 'medium' }).format(date)}
    </span>
  );
}

export function SimpleTokens() {
  const { t, i18n } = useTranslation();
  const { apiKeyPermissions } = usePermissions();
  const canWrite = apiKeyPermissions.canWrite;
  const { data: settings } = useGeneralSettings();
  const creditLabel = settings?.creditDisplayName?.trim() || DEFAULT_CREDIT_DISPLAY_NAME;

  const [search, setSearch] = useState('');
  const [cursors, setCursors] = useState<string[]>([]);
  const after = cursors[cursors.length - 1];
  const tokens = useSimpleTokens({ first: PAGE_SIZE, after, nameContainsFold: search.trim() || undefined });

  const createToken = useCreateApiKey();
  const updateToken = useUpdateApiKey();
  const updateProfiles = useUpdateApiKeyProfiles();
  const updateStatus = useUpdateApiKeyStatus();
  const rotateToken = useRotateApiKey();

  const [formOpen, setFormOpen] = useState(false);
  const [editing, setEditing] = useState<SimpleToken | null>(null);
  const [modelGroupID, setModelGroupID] = useState('');
  const groups = useAssignableModelGroups();
  const creationCatalog = useApiKeyCreationCatalog(modelGroupID || undefined);
  const editCatalog = useApiKeyProfileCatalog(editing?.id);
  const catalog = editing ? editCatalog : creationCatalog;
  const models = useMemo(
    () => (catalog.data?.configured ? catalog.data.models : []).map((model) => ({ modelId: model.id, name: model.name })),
    [catalog.data]
  );
  const initialForm = useMemo(() => (editing ? formFromToken(editing) : undefined), [editing]);
  const [created, setCreated] = useState<{ name: string; key: string; model?: string } | null>(null);
  const [detail, setDetail] = useState<SimpleToken | null>(null);
  const [confirm, setConfirm] = useState<{ kind: 'rotate' | 'delete'; token: SimpleToken } | null>(null);

  const rows = tokens.data?.edges.map((edge) => edge.node) ?? [];
  const total = tokens.data?.totalCount ?? 0;
  const defaultName = t('simple.tokens.defaultName', { index: total + 1 });
  const profileName = t('apikeys.access.defaultProfileName');

  const openCreate = () => {
    setEditing(null);
    setModelGroupID('');
    setFormOpen(true);
  };

  const submitForm = async (form: SimpleTokenForm) => {
    if (editing) {
      const update = planSimpleTokenUpdate(editing, form, profileName);
      if (update.name) await updateToken.mutateAsync({ id: editing.id, input: update.name });
      if (update.profiles) await updateProfiles.mutateAsync({ id: editing.id, input: update.profiles });
      setFormOpen(false);
      return;
    }
    const result = await createToken.mutateAsync(buildSimpleTokenCreation(form, profileName, groups.data ?? [], creationCatalog.data));
    setFormOpen(false);
    setCreated({ name: result.createAPIKey.name, key: result.createAPIKey.key, model: form.modelIds[0] ?? models[0]?.modelId });
  };

  const runConfirm = async () => {
    if (!confirm) return;
    if (confirm.kind === 'rotate') {
      const rotated = await rotateToken.mutateAsync(confirm.token.id);
      setCreated({ name: rotated.name, key: rotated.key, model: activeProfileOf(confirm.token)?.modelIDs?.[0] ?? models[0]?.modelId });
    } else {
      // 令牌不做物理删除：归档后不再出现在列表里，也不能再调用
      await updateStatus.mutateAsync({ id: confirm.token.id, status: 'archived' });
    }
    setConfirm(null);
  };

  return (
    <>
      <Header fixed>
        <PageHeading
          title={t('simple.tokens.title')}
          description={t('simple.tokens.description')}
          action={
            canWrite && (
              <Button onClick={openCreate}>
                <IconPlus className='size-4' /> {t('simple.tokens.create')}
              </Button>
            )
          }
        />
      </Header>
      <Main>
        <Card>
          <CardContent className='space-y-4 pt-6'>
            <Input
              value={search}
              onChange={(e) => {
                setSearch(e.target.value);
                setCursors([]);
              }}
              placeholder={t('simple.tokens.search')}
              className='max-w-xs'
            />
            {tokens.isLoading ? (
              <LoadingRows />
            ) : tokens.isError ? (
              <ErrorState onRetry={() => tokens.refetch()} />
            ) : rows.length === 0 ? (
              <EmptyState
                title={search ? t('simple.tokens.noMatch') : t('simple.tokens.emptyTitle')}
                description={search ? undefined : t('simple.tokens.emptyDescription')}
                action={
                  !search && canWrite ? (
                    <Button onClick={openCreate}>
                      <IconPlus className='size-4' /> {t('simple.tokens.create')}
                    </Button>
                  ) : undefined
                }
              />
            ) : (
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>{t('simple.tokens.columns.name')}</TableHead>
                    <TableHead>{t('simple.tokens.columns.usage')}</TableHead>
                    <TableHead>{t('simple.tokens.columns.models')}</TableHead>
                    <TableHead>{t('simple.tokens.columns.expiry')}</TableHead>
                    <TableHead>{t('simple.tokens.columns.createdAt')}</TableHead>
                    <TableHead className='w-24 text-right'>{t('simple.tokens.columns.actions')}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {rows.map((token) => {
                    const disabled = token.status === 'disabled';
                    return (
                      <TableRow key={token.id} className={cn('cursor-pointer', disabled && 'opacity-60')} onClick={() => setDetail(token)}>
                        <TableCell>
                          <div className='flex items-center gap-2'>
                            <span className='font-medium'>{token.name}</span>
                            {disabled && <Badge variant='secondary'>{t('simple.tokens.disabled')}</Badge>}
                          </div>
                          <div className='text-muted-foreground font-mono text-xs'>{maskKey(token.key)}</div>
                        </TableCell>
                        <TableCell>
                          <TokenUsageCell token={token} creditLabel={creditLabel} />
                        </TableCell>
                        <TableCell>
                          <ModelsCell token={token} />
                        </TableCell>
                        <TableCell>
                          <ExpiryCell token={token} />
                        </TableCell>
                        <TableCell>
                          <span className='text-sm' title={formatFullTime(token.createdAt, i18n.language)}>
                            {formatRelativeTime(token.createdAt, i18n.language, t)}
                          </span>
                        </TableCell>
                        <TableCell className='text-right' onClick={(e) => e.stopPropagation()}>
                          <div className='flex items-center justify-end gap-1'>
                            <Button
                              variant='ghost'
                              size='icon'
                              aria-label={t('simple.tokens.copy')}
                              onClick={() => copyText(token.key, t('simple.tokens.copied'))}
                            >
                              <IconCopy className='size-4' />
                            </Button>
                            {canWrite && (
                              <DropdownMenu>
                                <DropdownMenuTrigger asChild>
                                  <Button variant='ghost' size='icon' aria-label={t('simple.tokens.more')}>
                                    <IconDots className='size-4' />
                                  </Button>
                                </DropdownMenuTrigger>
                                <DropdownMenuContent align='end'>
                                  <DropdownMenuItem
                                    onClick={() => {
                                      setEditing(token);
                                      setFormOpen(true);
                                    }}
                                  >
                                    {t('common.buttons.edit')}
                                  </DropdownMenuItem>
                                  <DropdownMenuItem
                                    onClick={() => updateStatus.mutate({ id: token.id, status: disabled ? 'enabled' : 'disabled' })}
                                  >
                                    {disabled ? t('common.buttons.enable') : t('common.buttons.disable')}
                                  </DropdownMenuItem>
                                  <DropdownMenuItem onClick={() => setConfirm({ kind: 'rotate', token })}>
                                    {t('simple.tokens.rotate')}
                                  </DropdownMenuItem>
                                  <DropdownMenuSeparator />
                                  <DropdownMenuItem className='text-red-600' onClick={() => setConfirm({ kind: 'delete', token })}>
                                    {t('common.buttons.delete')}
                                  </DropdownMenuItem>
                                </DropdownMenuContent>
                              </DropdownMenu>
                            )}
                          </div>
                        </TableCell>
                      </TableRow>
                    );
                  })}
                </TableBody>
              </Table>
            )}
            {(cursors.length > 0 || tokens.data?.pageInfo.hasNextPage) && (
              <div className='flex items-center justify-end gap-2'>
                <Button variant='outline' size='sm' disabled={cursors.length === 0} onClick={() => setCursors((c) => c.slice(0, -1))}>
                  {t('simple.pager.previous')}
                </Button>
                <Button
                  variant='outline'
                  size='sm'
                  disabled={!tokens.data?.pageInfo.hasNextPage}
                  onClick={() => {
                    const end = tokens.data?.pageInfo.endCursor;
                    if (end) setCursors((c) => [...c, end]);
                  }}
                >
                  {t('simple.pager.next')}
                </Button>
              </div>
            )}
          </CardContent>
        </Card>
      </Main>

      <TokenFormDialog
        open={formOpen}
        onOpenChange={setFormOpen}
        mode={editing ? 'edit' : 'create'}
        initial={initialForm}
        modelGroupID={editing ? (editing.modelGroupID ?? '') : modelGroupID}
        groups={groups.data ?? []}
        onModelGroupChange={setModelGroupID}
        catalogReady={!!catalog.data?.configured && !catalog.isFetching}
        catalogError={catalog.isError}
        nameOnly={!!editing && (hasAdvancedSettings(editing) || !catalog.data?.configured || catalog.isFetching || catalog.isError)}
        defaultName={defaultName}
        models={models}
        creditLabel={creditLabel}
        submitting={createToken.isPending || updateToken.isPending || updateProfiles.isPending}
        onSubmit={(form) => void submitForm(form).catch(() => undefined)}
      />

      <Dialog open={!!created} onOpenChange={(open) => !open && setCreated(null)}>
        <DialogContent className='sm:max-w-2xl'>
          <DialogHeader>
            <DialogTitle>{t('simple.tokens.created.title', { name: created?.name ?? '' })}</DialogTitle>
            <DialogDescription>{t('simple.tokens.created.description')}</DialogDescription>
          </DialogHeader>
          {created && (
            <div className='space-y-4'>
              <div className='bg-muted flex items-center gap-2 rounded-md p-3'>
                <code className='flex-1 font-mono text-sm break-all'>{created.key}</code>
                <CopyButton content={created.key} copyMessage={t('simple.tokens.copied')} />
              </div>
              <UsageExamples apiKey={created.key} model={created.model} />
            </div>
          )}
          <DialogFooter>
            <Button onClick={() => setCreated(null)}>{t('simple.tokens.created.done')}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Sheet open={!!detail} onOpenChange={(open) => !open && setDetail(null)}>
        <SheetContent className='w-full overflow-y-auto sm:max-w-xl'>
          {detail && (
            <>
              <SheetHeader>
                <SheetTitle>{detail.name}</SheetTitle>
                <SheetDescription>{t('simple.tokens.detail.description')}</SheetDescription>
              </SheetHeader>
              <div className='space-y-6 px-4 pb-6'>
                <div className='bg-muted flex items-center gap-2 rounded-md p-3'>
                  <code className='flex-1 font-mono text-sm'>{maskKey(detail.key)}</code>
                  <CopyButton content={detail.key} copyMessage={t('simple.tokens.copied')} />
                </div>
                <dl className='grid grid-cols-2 gap-4 text-sm'>
                  <div>
                    <dt className='text-muted-foreground'>{t('simple.tokens.columns.usage')}</dt>
                    <dd className='mt-1'>
                      <TokenUsageCell token={detail} creditLabel={creditLabel} />
                    </dd>
                  </div>
                  <div>
                    <dt className='text-muted-foreground'>{t('simple.tokens.columns.expiry')}</dt>
                    <dd className='mt-1'>
                      <ExpiryCell token={detail} />
                    </dd>
                  </div>
                  <div className='col-span-2'>
                    <dt className='text-muted-foreground'>{t('simple.tokens.columns.models')}</dt>
                    <dd className='mt-1 font-mono text-xs break-words'>
                      {(activeProfileOf(detail)?.modelIDs ?? []).join(', ') || t('simple.tokens.allModels')}
                    </dd>
                  </div>
                </dl>
                {hasAdvancedSettings(detail) && <p className='text-muted-foreground text-xs'>{t('simple.tokens.advancedNotice')}</p>}
                <div className='space-y-2'>
                  <h3 className='text-sm font-medium'>{t('simple.tokens.detail.examples')}</h3>
                  <UsageExamples apiKey={detail.key} model={activeProfileOf(detail)?.modelIDs?.[0] ?? models[0]?.modelId} />
                </div>
              </div>
            </>
          )}
        </SheetContent>
      </Sheet>

      <ConfirmDialog
        open={!!confirm}
        onOpenChange={(open) => !open && setConfirm(null)}
        title={confirm?.kind === 'rotate' ? t('simple.tokens.confirmRotate.title') : t('simple.tokens.confirmDelete.title')}
        desc={
          confirm?.kind === 'rotate'
            ? t('simple.tokens.confirmRotate.description', { name: confirm?.token.name ?? '' })
            : t('simple.tokens.confirmDelete.description', { name: confirm?.token.name ?? '' })
        }
        confirmText={confirm?.kind === 'rotate' ? t('simple.tokens.rotate') : t('common.buttons.delete')}
        destructive
        isLoading={rotateToken.isPending || updateStatus.isPending}
        handleConfirm={() => void runConfirm().catch(() => undefined)}
      />
    </>
  );
}
