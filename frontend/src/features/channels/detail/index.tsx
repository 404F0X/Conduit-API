import type React from 'react';
import { lazy, Suspense } from 'react';
import { format } from 'date-fns';
import { Link, useNavigate } from '@tanstack/react-router';
import { IconArrowLeft, IconEdit, IconPlayerPlay } from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { cn } from '@/lib/utils';
import { usePermissions } from '@/hooks/usePermissions';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { Header } from '@/components/layout/header';
import { Main } from '@/components/layout/main';
import { EmptyState, ErrorState, LoadingRows } from '@/components/page-state';
import ChannelsProvider, { useChannels } from '../context/channels-context';
import { useQueryChannels, useTestChannel } from '../data/channels';
import type { Channel } from '../data/schema';

const ChannelsDialogs = lazy(() => import('../components/channels-dialogs').then((m) => ({ default: m.ChannelsDialogs })));

type DialogName = Parameters<ReturnType<typeof useChannels>['setOpen']>[0];

/** 一行“说明 + 按钮”，每个按钮打开原有的设置对话框（逻辑不变，只是从行菜单搬到了详情页的标签里）。 */
function SettingRow({ title, description, action }: { title: string; description?: string; action: React.ReactNode }) {
  return (
    <div className='flex flex-col gap-2 border-b py-3 last:border-b-0 sm:flex-row sm:items-center sm:justify-between'>
      <div className='min-w-0'>
        <p className='text-sm font-medium'>{title}</p>
        {description && <p className='text-muted-foreground text-xs'>{description}</p>}
      </div>
      <div className='shrink-0'>{action}</div>
    </div>
  );
}

function Field({ label, children, wide }: { label: string; children: React.ReactNode; wide?: boolean }) {
  return (
    <div className={cn('min-w-0', wide && 'sm:col-span-2')}>
      <dt className='text-muted-foreground text-xs'>{label}</dt>
      <dd className='mt-1 text-sm break-words'>{children}</dd>
    </div>
  );
}

function ChannelDetailBody({ channel }: { channel: Channel }) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const { setOpen, setCurrentRow, setSelectedPriceModelID } = useChannels();
  const { channelPermissions, hasSystemScope } = usePermissions();
  const canWrite = channelPermissions.canWrite;
  const testChannel = useTestChannel();
  const apiKeysCount = channel.credentials?.apiKeys?.filter((key) => key.trim().length > 0).length ?? 0;
  const disabledKeysCount = channel.disabledAPIKeys?.length ?? 0;
  const isArchived = channel.status === 'archived';

  const open = (dialog: DialogName) => {
    setCurrentRow(channel);
    if (dialog === 'price') setSelectedPriceModelID(null);
    setOpen(dialog);
  };
  const button = (label: string, dialog: DialogName, variant: 'outline' | 'default' = 'outline') => (
    <Button size='sm' variant={variant} onClick={() => open(dialog)} disabled={!canWrite}>
      {label}
    </Button>
  );

  return (
    <>
      <Header fixed>
        <div className='flex min-w-0 flex-1 items-center justify-between gap-4'>
          <div className='flex min-w-0 items-center gap-3'>
            <Button variant='ghost' size='icon' asChild aria-label={t('channels.detail.back')}>
              <Link to='/channels'>
                <IconArrowLeft className='size-4' />
              </Link>
            </Button>
            <div className='min-w-0'>
              <div className='flex items-center gap-2'>
                <h2 className='truncate text-xl font-bold tracking-tight'>{channel.name}</h2>
                <Badge variant={channel.status === 'enabled' ? 'default' : 'secondary'}>{t(`channels.status.${channel.status}`)}</Badge>
              </div>
              <p className='text-muted-foreground truncate text-sm'>{t(`channels.types.${channel.type}`)}</p>
            </div>
          </div>
          <div className='flex shrink-0 items-center gap-2'>
            {canWrite && (
              <Button variant='outline' onClick={() => open('edit')}>
                <IconEdit className='size-4' /> {t('common.buttons.edit')}
              </Button>
            )}
            <Button
              onClick={() =>
                testChannel.mutateAsync({ channelID: channel.id, modelID: channel.defaultTestModel || undefined }).catch(() => undefined)
              }
              disabled={testChannel.isPending}
            >
              <IconPlayerPlay className='size-4' /> {t('channels.actions.test')}
            </Button>
          </div>
        </div>
      </Header>

      <Main>
        <Tabs defaultValue='overview' className='w-full'>
          <TabsList className='flex h-auto flex-wrap justify-start'>
            <TabsTrigger value='overview'>{t('channels.detail.tabs.overview')}</TabsTrigger>
            <TabsTrigger value='keys'>{t('channels.detail.tabs.keys')}</TabsTrigger>
            <TabsTrigger value='models'>{t('channels.detail.tabs.models')}</TabsTrigger>
            {(hasSystemScope('write_commercialization') || hasSystemScope('read_commercialization')) && (
              <TabsTrigger value='pricing'>{t('channels.detail.tabs.pricing')}</TabsTrigger>
            )}
            <TabsTrigger value='limits'>{t('channels.detail.tabs.limits')}</TabsTrigger>
            <TabsTrigger value='network'>{t('channels.detail.tabs.network')}</TabsTrigger>
            <TabsTrigger value='advanced'>{t('channels.detail.tabs.advanced')}</TabsTrigger>
          </TabsList>

          <TabsContent value='overview' className='space-y-4'>
            {channel.errorMessage && (
              <Card className='border-destructive/40'>
                <CardContent className='flex flex-col gap-3 pt-6 sm:flex-row sm:items-center sm:justify-between'>
                  <p className='text-destructive text-sm'>
                    {t(`channels.messages.${channel.errorMessage}`, { defaultValue: channel.errorMessage })}
                  </p>
                  {button(t('channels.actions.markErrorResolved'), 'errorResolved')}
                </CardContent>
              </Card>
            )}
            <Card>
              <CardContent className='pt-6'>
                <dl className='grid gap-4 sm:grid-cols-2'>
                  <Field label={t('channels.detail.fields.baseURL')}>
                    <span className='font-mono text-xs'>{channel.baseURL}</span>
                  </Field>
                  <Field label={t('channels.detail.fields.defaultTestModel')}>
                    <span className='font-mono text-xs'>{channel.defaultTestModel || '—'}</span>
                  </Field>
                  <Field label={t('channels.detail.fields.models')}>
                    {t('channels.detail.modelCount', { count: channel.supportedModels.length })}
                  </Field>
                  <Field label={t('channels.detail.fields.keys')}>
                    {t('channels.detail.keyCount', { count: apiKeysCount })}
                    {disabledKeysCount > 0 && (
                      <span className='ml-2 text-amber-600'>{t('channels.detail.disabledKeyCount', { count: disabledKeysCount })}</span>
                    )}
                  </Field>
                  <Field label={t('channels.detail.fields.tags')}>{channel.tags?.length ? channel.tags.join('、') : '—'}</Field>
                  <Field label={t('channels.detail.fields.createdAt')}>{format(new Date(channel.createdAt), 'yyyy-MM-dd HH:mm')}</Field>
                  {channel.remark && (
                    <Field label={t('channels.detail.fields.remark')} wide>
                      {channel.remark}
                    </Field>
                  )}
                </dl>
              </CardContent>
            </Card>
            <Card>
              <CardContent className='pt-2'>
                <SettingRow
                  title={t('channels.operations.action')}
                  description={t('channels.detail.hints.operations')}
                  action={
                    <Button size='sm' variant='outline' onClick={() => open('operations')}>
                      {t('channels.detail.open')}
                    </Button>
                  }
                />
                <SettingRow
                  title={t('channels.actions.testHistory')}
                  description={t('channels.detail.hints.testHistory')}
                  action={
                    <Button size='sm' variant='outline' onClick={() => open('testHistory')}>
                      {t('channels.detail.open')}
                    </Button>
                  }
                />
              </CardContent>
            </Card>
          </TabsContent>

          <TabsContent value='keys'>
            <Card>
              <CardHeader>
                <CardTitle className='text-base'>{t('channels.detail.keyCount', { count: apiKeysCount })}</CardTitle>
                <CardDescription>{t('channels.detail.hints.keys')}</CardDescription>
              </CardHeader>
              <CardContent>
                <SettingRow
                  title={t('channels.detail.editKeys')}
                  description={t('channels.detail.hints.editKeys')}
                  action={button(t('common.buttons.edit'), 'edit')}
                />
                {apiKeysCount > 1 && (
                  <SettingRow
                    title={t('channels.actions.testAPIKeys', { count: apiKeysCount })}
                    description={t('channels.detail.hints.testKeys')}
                    action={button(t('channels.actions.test'), 'testAPIKeys')}
                  />
                )}
                {disabledKeysCount > 0 && (
                  <SettingRow
                    title={t('channels.actions.disabledAPIKeys', { count: disabledKeysCount })}
                    description={t('channels.detail.hints.disabledKeys')}
                    action={button(t('channels.detail.open'), 'disabledAPIKeys')}
                  />
                )}
              </CardContent>
            </Card>
          </TabsContent>

          <TabsContent value='models'>
            <Card>
              <CardHeader>
                <CardTitle className='text-base'>{t('channels.detail.modelCount', { count: channel.supportedModels.length })}</CardTitle>
                <CardDescription>
                  {channel.autoSyncSupportedModels ? t('channels.detail.hints.autoSyncOn') : t('channels.detail.hints.autoSyncOff')}
                </CardDescription>
              </CardHeader>
              <CardContent className='space-y-4'>
                <div className='flex max-h-48 flex-wrap gap-1 overflow-y-auto'>
                  {channel.supportedModels.map((model) => (
                    <Badge key={model} variant='outline' className='font-mono text-xs'>
                      {model}
                    </Badge>
                  ))}
                  {channel.supportedModels.length === 0 && <p className='text-muted-foreground text-sm'>{t('channels.detail.noModels')}</p>}
                </div>
                <div>
                  <SettingRow
                    title={t('channels.detail.editModels')}
                    description={t('channels.detail.hints.editModels')}
                    action={button(t('common.buttons.edit'), 'viewModels')}
                  />
                  <SettingRow
                    title={t('channels.dialogs.settings.modelMapping.title')}
                    description={t('channels.detail.hints.modelMapping')}
                    action={button(t('channels.detail.open'), 'modelMapping')}
                  />
                </div>
              </CardContent>
            </Card>
          </TabsContent>

          <TabsContent value='pricing'>
            <Card>
              <CardContent className='pt-2'>
                {hasSystemScope('write_commercialization') && (
                  <SettingRow
                    title={t('channels.actions.modelPrice')}
                    description={t('channels.detail.hints.price')}
                    action={button(t('channels.detail.open'), 'price')}
                  />
                )}
                {hasSystemScope('read_commercialization') && (
                  <SettingRow
                    title={t('channels.changeSets.action')}
                    description={t('channels.detail.hints.changeSets')}
                    action={
                      <Button
                        size='sm'
                        variant='outline'
                        onClick={() => navigate({ to: '/change-sets', search: { scopeType: 'channel', scopeID: channel.id } })}
                      >
                        {t('channels.detail.open')}
                      </Button>
                    }
                  />
                )}
                <p className='text-muted-foreground pt-3 text-xs'>{t('channels.detail.hints.pricingHome')}</p>
              </CardContent>
            </Card>
          </TabsContent>

          <TabsContent value='limits'>
            <Card>
              <CardContent className='pt-2'>
                <SettingRow
                  title={t('channels.dialogs.rateLimit.action')}
                  description={t('channels.detail.hints.rateLimit')}
                  action={button(t('channels.detail.open'), 'rateLimit')}
                />
                {hasSystemScope('read_settings') && (
                  <SettingRow
                    title={t('channels.loadBalancingStrategy')}
                    description={t('channels.detail.hints.retry')}
                    action={
                      <Button size='sm' variant='outline' onClick={() => navigate({ to: '/system', search: { tab: 'retry' } })}>
                        {t('channels.detail.open')}
                      </Button>
                    }
                  />
                )}
              </CardContent>
            </Card>
          </TabsContent>

          <TabsContent value='network'>
            <Card>
              <CardContent className='pt-2'>
                <SettingRow
                  title={t('channels.dialogs.proxy.action')}
                  description={t('channels.detail.hints.proxy')}
                  action={button(t('channels.detail.open'), 'proxy')}
                />
                <SettingRow
                  title={t('channels.endpoints.title')}
                  description={t('channels.detail.hints.endpoints')}
                  action={button(t('channels.detail.open'), 'endpoints')}
                />
                <SettingRow
                  title={t('channels.actions.queryUpstreamQuota')}
                  description={t('channels.detail.hints.quota')}
                  action={button(t('channels.detail.open'), 'quotaProbe')}
                />
              </CardContent>
            </Card>
          </TabsContent>

          <TabsContent value='advanced' className='space-y-4'>
            <Card>
              <CardContent className='pt-2'>
                <SettingRow
                  title={t('channels.dialogs.settings.overrides.action')}
                  description={t('channels.detail.hints.overrides')}
                  action={button(t('channels.detail.open'), 'overrides')}
                />
                <SettingRow
                  title={t('channels.dialogs.transformOptions.action')}
                  description={t('channels.detail.hints.transform')}
                  action={button(t('channels.detail.open'), 'transformOptions')}
                />
                <SettingRow
                  title={t('channels.dialogs.automation.action')}
                  description={t('channels.detail.hints.automation')}
                  action={button(t('channels.detail.open'), 'automation')}
                />
                <SettingRow
                  title={t('common.actions.duplicate')}
                  description={t('channels.detail.hints.duplicate')}
                  action={button(t('common.actions.duplicate'), 'duplicate')}
                />
              </CardContent>
            </Card>
            {canWrite && (
              <Card className='border-destructive/30'>
                <CardContent className='pt-2'>
                  <SettingRow
                    title={t(isArchived ? 'common.buttons.restore' : 'common.buttons.archive')}
                    description={isArchived ? t('channels.detail.hints.restore') : t('channels.detail.hints.archive')}
                    action={button(t(isArchived ? 'common.buttons.restore' : 'common.buttons.archive'), 'archive')}
                  />
                  <SettingRow
                    title={t('common.buttons.delete')}
                    description={t('channels.detail.hints.delete')}
                    action={
                      <Button size='sm' variant='destructive' onClick={() => open('delete')}>
                        {t('common.buttons.delete')}
                      </Button>
                    }
                  />
                </CardContent>
              </Card>
            )}
          </TabsContent>
        </Tabs>
      </Main>
    </>
  );
}

function ChannelDetailLoader({ channelId }: { channelId: string }) {
  const { t } = useTranslation();
  const query = useQueryChannels({ first: 1, where: { id: channelId } });
  const channel = query.data?.edges?.[0]?.node ?? null;

  if (query.isLoading) {
    return (
      <Main>
        <LoadingRows />
      </Main>
    );
  }
  if (query.isError) {
    return (
      <Main>
        <ErrorState onRetry={() => query.refetch()} />
      </Main>
    );
  }
  if (!channel) {
    return (
      <Main>
        <EmptyState
          title={t('channels.detail.notFound')}
          action={
            <Button asChild variant='outline'>
              <Link to='/channels'>{t('channels.detail.back')}</Link>
            </Button>
          }
        />
      </Main>
    );
  }
  return <ChannelDetailBody channel={channel} />;
}

/** 渠道详情页：把原来行菜单里的 19 项操作按用途分到 7 个标签里。 */
export function ChannelDetail({ channelId }: { channelId: string }) {
  return (
    <ChannelsProvider>
      <ChannelDetailLoader channelId={channelId} />
      <Suspense fallback={null}>
        <ChannelsDialogs />
      </Suspense>
    </ChannelsProvider>
  );
}
