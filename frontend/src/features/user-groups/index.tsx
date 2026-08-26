import { useMemo, useState, type ReactNode } from 'react';
import {
  IconAlertTriangle,
  IconArchive,
  IconBox,
  IconEdit,
  IconLoader2,
  IconPlus,
  IconRefresh,
  IconRoute,
  IconSearch,
  IconUsers,
  IconUsersGroup,
} from '@tabler/icons-react';
import { toast } from 'sonner';
import { usePermissions } from '@/hooks/usePermissions';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/checkbox';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { ScrollArea } from '@/components/ui/scroll-area';
import { Skeleton } from '@/components/ui/skeleton';
import { Switch } from '@/components/ui/switch';
import { Textarea } from '@/components/ui/textarea';
import { ConfirmDialog } from '@/components/confirm-dialog';
import { Header } from '@/components/layout/header';
import { Main } from '@/components/layout/main';
import { useProductExperience } from '@/features/product-experience';
import {
  type GroupModelOption,
  type GroupRouteOption,
  type GroupUserOption,
  type SimpleGroup,
  type SimpleGroupStatus,
  useArchiveSimpleGroup,
  useCreateSimpleGroup,
  useSimpleGroupModelsCatalog,
  useSimpleGroupRoutesCatalog,
  useSimpleGroups,
  useSimpleGroupUsersCatalog,
  useUpdateSimpleGroup,
} from './data';

const PPM = 1_000_000;

type EditableStatus = Exclude<SimpleGroupStatus, 'ARCHIVED'>;

type GroupDraft = {
  name: string;
  description: string;
  status: EditableStatus;
  isDefault: boolean;
  modelIDs: string[];
  routeIDs: string[];
  memberUserIDs: string[];
  multiplier: string;
};

const emptyDraft = (): GroupDraft => ({
  name: '',
  description: '',
  status: 'ENABLED',
  isDefault: false,
  modelIDs: [],
  routeIDs: [],
  memberUserIDs: [],
  multiplier: '1',
});

const entityID = (id: string) => id.split('/').pop() || id;

function toggle(values: string[], value: string, checked: boolean) {
  return checked ? [...new Set([...values, value])] : values.filter((item) => item !== value);
}

function formatMultiplier(multiplierPpm: number) {
  const whole = Math.floor(multiplierPpm / PPM);
  const fraction = String(multiplierPpm % PPM)
    .padStart(6, '0')
    .replace(/0+$/, '');
  return fraction ? `${whole}.${fraction}` : String(whole);
}

function parseMultiplier(value: string): number | null {
  const normalized = value.trim().startsWith('.') ? `0${value.trim()}` : value.trim();
  const match = /^(\d+)(?:\.(\d*))?$/.exec(normalized);
  if (!match) return null;

  const extraDecimals = (match[2] || '').slice(6);
  if (extraDecimals && !/^0+$/.test(extraDecimals)) return null;

  const whole = Number(match[1]);
  const fraction = Number((match[2] || '').slice(0, 6).padEnd(6, '0'));
  const result = whole * PPM + fraction;
  return Number.isSafeInteger(result) ? result : null;
}

export default function SimpleGroupsPage() {
  const { hasSystemScope } = usePermissions();
  const { mode } = useProductExperience();
  const canWrite = hasSystemScope('write_groups');
  const canReadUsers = hasSystemScope('read_users');
  const canReadModels = hasSystemScope('read_channels');
  const groupsQuery = useSimpleGroups();
  const createGroup = useCreateSimpleGroup();
  const updateGroup = useUpdateSimpleGroup();
  const archiveGroup = useArchiveSimpleGroup();
  const [editor, setEditor] = useState<SimpleGroup | null | undefined>(undefined);
  const [archiveTarget, setArchiveTarget] = useState<SimpleGroup | null>(null);
  const [draft, setDraft] = useState<GroupDraft>(emptyDraft());
  const [modelSearch, setModelSearch] = useState('');
  const [routeSearch, setRouteSearch] = useState('');
  const [userSearch, setUserSearch] = useState('');
  const [formError, setFormError] = useState<string | null>(null);

  const editorOpen = editor !== undefined;
  const usersQuery = useSimpleGroupUsersCatalog(editorOpen && canReadUsers);
  const modelsQuery = useSimpleGroupModelsCatalog(editorOpen && canReadModels);
  const routesQuery = useSimpleGroupRoutesCatalog(editorOpen && canReadModels && mode === 'ENTERPRISE');
  const users = useMemo(() => usersQuery.data?.users.edges.map((edge) => edge.node) || [], [usersQuery.data]);
  const models = useMemo(() => modelsQuery.data?.models.edges.map((edge) => edge.node) || [], [modelsQuery.data]);
  const routes = useMemo(() => routesQuery.data?.modelRoutes || [], [routesQuery.data]);
  const groups = groupsQuery.data?.simpleGroups || [];
  const isSaving = createGroup.isPending || updateGroup.isPending;

  const openCreate = () => {
    setDraft(emptyDraft());
    setModelSearch('');
    setRouteSearch('');
    setUserSearch('');
    setFormError(null);
    setEditor(null);
  };

  const openEdit = (group: SimpleGroup) => {
    if (group.status === 'ARCHIVED') return;
    setDraft({
      name: group.name,
      description: group.description || '',
      status: group.status,
      isDefault: group.isDefault,
      modelIDs: group.modelIDs.map(entityID),
      routeIDs: group.routeIDs.map(entityID),
      memberUserIDs: group.memberUserIDs.map(entityID),
      multiplier: formatMultiplier(group.multiplierPpm),
    });
    setModelSearch('');
    setRouteSearch('');
    setUserSearch('');
    setFormError(null);
    setEditor(group);
  };

  const closeEditor = () => {
    if (!isSaving) setEditor(undefined);
  };

  const submit = async () => {
    const name = draft.name.trim();
    const description = draft.description.trim();
    const multiplierPpm = parseMultiplier(draft.multiplier);
    const modelsReady = canReadModels && modelsQuery.isSuccess;
    const routesReady = mode === 'ENTERPRISE' && canReadModels && routesQuery.isSuccess;
    const usersReady = canReadUsers && usersQuery.isSuccess;

    if (!name) {
      setFormError('请输入模型组名称。');
      return;
    }
    if (multiplierPpm === null) {
      setFormError('倍率必须是大于或等于 0、最多六位小数的数字。');
      return;
    }
    if (!editor && !modelsReady) {
      setFormError('创建模型组需要先读取模型目录，请检查 read_channels 权限或稍后重试。');
      return;
    }
    if (!editor && draft.modelIDs.length === 0) {
      setFormError('创建模型组至少需要选择一个可用模型。');
      return;
    }

    setFormError(null);
    try {
      if (editor) {
        await updateGroup.mutateAsync({
          groupID: editor.id,
          name,
          ...(description ? { description } : { clearDescription: true }),
          status: draft.status,
          isDefault: draft.isDefault,
          multiplierPpm,
          ...(modelsReady ? { modelIDs: draft.modelIDs } : {}),
          ...(routesReady ? { routeIDs: draft.routeIDs } : {}),
          ...(usersReady && draft.status === 'ENABLED' ? { userIDs: draft.memberUserIDs } : {}),
        });
        toast.success('模型组已更新');
      } else {
        await createGroup.mutateAsync({
          name,
          ...(description ? { description } : {}),
          isDefault: draft.isDefault,
          modelIDs: draft.modelIDs,
          routeIDs: mode === 'ENTERPRISE' ? draft.routeIDs : [],
          multiplierPpm,
          ...(usersReady ? { userIDs: draft.memberUserIDs } : {}),
        });
        toast.success('模型组已创建');
      }
      setEditor(undefined);
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : '保存模型组失败';
      setFormError(message);
      toast.error(message);
    }
  };

  const archive = async () => {
    if (!archiveTarget) return;
    try {
      await archiveGroup.mutateAsync(archiveTarget.id);
      toast.success(`“${archiveTarget.name}”已归档`);
      setArchiveTarget(null);
    } catch (cause) {
      toast.error(cause instanceof Error ? cause.message : '归档模型组失败');
    }
  };

  return (
    <>
      <Header fixed>
        <div className='flex flex-1 items-center justify-between gap-4'>
          <div className='min-w-0'>
            <h1 className='flex items-center gap-2 text-xl font-semibold tracking-tight'>
              <IconUsersGroup className='text-primary size-5' />
              模型组
            </h1>
            <p className='text-muted-foreground mt-0.5 hidden truncate text-sm sm:block'>
              为用户统一配置可调用的对外模型、零售倍率和订阅访问范围。
            </p>
          </div>
          <Button
            onClick={openCreate}
            disabled={!canWrite || createGroup.isPending}
            title={canWrite ? undefined : '需要 write_groups 权限'}
          >
            <IconPlus />
            新建模型组
          </Button>
        </div>
      </Header>

      <Main className='space-y-4 pb-10'>
        {groupsQuery.isLoading ? (
          <GroupsLoading />
        ) : groupsQuery.isError ? (
          <GroupsError message={groupsQuery.error.message} onRetry={() => void groupsQuery.refetch()} />
        ) : groups.length === 0 ? (
          <EmptyGroups canWrite={canWrite} onCreate={openCreate} />
        ) : (
          <>
            <GroupOverview groups={groups} />
            <section className='bg-card overflow-hidden rounded-lg border' aria-label='模型组列表'>
              <div className='divide-y'>
                {groups.map((group) => (
                  <GroupRow
                    key={group.id}
                    group={group}
                    enterprise={mode === 'ENTERPRISE'}
                    canWrite={canWrite}
                    archivePending={archiveGroup.isPending && archiveTarget?.id === group.id}
                    onEdit={() => openEdit(group)}
                    onArchive={() => setArchiveTarget(group)}
                  />
                ))}
              </div>
            </section>
          </>
        )}
      </Main>

      <Dialog open={editorOpen} onOpenChange={(open) => !open && closeEditor()}>
        <DialogContent
          className='h-[min(92vh,60rem)] max-h-[92vh] max-w-[calc(100vw-1.5rem)] grid-rows-[auto_minmax(0,1fr)_auto] gap-0 overflow-hidden p-0 sm:max-w-[calc(100vw-3rem)] xl:max-w-7xl'
          showCloseButton={!isSaving}
        >
          <DialogHeader className='border-b px-6 py-5 pr-12'>
            <DialogTitle>{editor ? `编辑“${editor.name}”` : '新建模型组'}</DialogTitle>
            <DialogDescription>对外模型 + 倍率 + 用户 = 一个模型组。底层访问计划和价格层级由系统自动维护。</DialogDescription>
          </DialogHeader>

          <ScrollArea className='min-h-0'>
            <div className='space-y-6 px-4 py-5 sm:px-6'>
              {formError && (
                <div
                  className='border-destructive/30 bg-destructive/5 text-destructive flex items-start gap-2 rounded-md border px-3 py-2.5 text-sm'
                  role='alert'
                >
                  <IconAlertTriangle className='mt-0.5 size-4 shrink-0' />
                  <span>{formError}</span>
                </div>
              )}

              <FormSection title='基本信息' description='给这个模型组一个清楚的名称，并决定它是否立即生效。'>
                <div className='grid gap-4 sm:grid-cols-2'>
                  <div className='space-y-2'>
                    <Label htmlFor='simple-group-name'>模型组名称</Label>
                    <Input
                      id='simple-group-name'
                      value={draft.name}
                      onChange={(event) => setDraft((current) => ({ ...current, name: event.target.value }))}
                      placeholder='例如：标准模型组'
                      autoFocus
                      aria-invalid={!!formError && !draft.name.trim()}
                    />
                  </div>
                  <div className='grid grid-cols-2 gap-3'>
                    <SettingToggle
                      label='启用'
                      hint={!editor ? '新模型组默认启用' : draft.isDefault ? '默认模型组必须启用' : '停用后暂停生效'}
                      checked={draft.status === 'ENABLED'}
                      disabled={!editor || draft.isDefault}
                      onChange={(checked) =>
                        setDraft((current) => ({
                          ...current,
                          status: checked ? 'ENABLED' : 'DISABLED',
                          memberUserIDs: !checked && editor ? editor.memberUserIDs.map(entityID) : current.memberUserIDs,
                        }))
                      }
                    />
                    <SettingToggle
                      label='默认模型组'
                      hint={editor?.isDefault ? '请用其他模型组替换默认模型组' : '未分配用户回到这里'}
                      checked={draft.isDefault}
                      disabled={!!editor?.isDefault}
                      onChange={(checked) =>
                        setDraft((current) => ({
                          ...current,
                          isDefault: checked,
                          status: checked ? 'ENABLED' : current.status,
                        }))
                      }
                    />
                  </div>
                </div>
                <div className='mt-4 space-y-2'>
                  <Label htmlFor='simple-group-description'>说明</Label>
                  <Textarea
                    id='simple-group-description'
                    value={draft.description}
                    onChange={(event) => setDraft((current) => ({ ...current, description: event.target.value }))}
                    placeholder='说明这个模型组适合哪些用户'
                    className='min-h-20 resize-y'
                  />
                </div>
              </FormSection>

              <FormSection title='访问范围' description='对外模型决定用户可以请求什么；企业模式还可限定这些模型允许使用的具体上游实例。'>
                {!canReadModels ? (
                  <CatalogWarning
                    message={
                      editor
                        ? '缺少 read_channels 权限，无法读取对外模型与上游实例目录；保存时会保留当前访问范围。'
                        : '缺少 read_channels 权限，无法选择必需的对外模型，因此暂时不能创建模型组。'
                    }
                  />
                ) : (
                  <div className='grid gap-4 xl:grid-cols-2'>
                    <PermissionPanel
                      step='1'
                      title='选择可调用的对外模型'
                      hint='先决定模型组内用户可以在 API 请求中使用哪些稳定的模型 ID。'
                      count={draft.modelIDs.length}
                    >
                      {modelsQuery.isError ? (
                        <CatalogWarning message='模型目录加载失败；保存时会保留当前模型。' onRetry={() => void modelsQuery.refetch()} />
                      ) : modelsQuery.isLoading ? (
                        <PickerLoading />
                      ) : (
                        <ModelPicker
                          models={models}
                          values={draft.modelIDs}
                          search={modelSearch}
                          onSearch={setModelSearch}
                          onChange={(modelIDs) => setDraft((current) => ({ ...current, modelIDs }))}
                        />
                      )}
                    </PermissionPanel>
                    {mode === 'ENTERPRISE' && (
                      <PermissionPanel
                        step='2'
                        title='可选：限定上游路由'
                        hint='再按需限定由哪些上游渠道和模型实例提供服务；不选择表示不限制。'
                        count={draft.routeIDs.length}
                        unrestricted={draft.routeIDs.length === 0}
                        advanced
                      >
                        {routesQuery.isError ? (
                          <CatalogWarning
                            message='上游映射目录加载失败；保存时会保留当前限制。'
                            onRetry={() => void routesQuery.refetch()}
                          />
                        ) : routesQuery.isLoading ? (
                          <PickerLoading />
                        ) : (
                          <RoutePicker
                            routes={routes}
                            values={draft.routeIDs}
                            search={routeSearch}
                            onSearch={setRouteSearch}
                            onChange={(routeIDs) => setDraft((current) => ({ ...current, routeIDs }))}
                          />
                        )}
                      </PermissionPanel>
                    )}
                  </div>
                )}
              </FormSection>

              <FormSection title='零售价格' description='倍率作用于对用户展示的零售价；1× 为原价，0.8× 为八折，1.2× 为加价 20%。'>
                <div className='max-w-xs space-y-2'>
                  <Label htmlFor='simple-group-multiplier'>价格倍率</Label>
                  <div className='relative'>
                    <Input
                      id='simple-group-multiplier'
                      inputMode='decimal'
                      value={draft.multiplier}
                      onChange={(event) => setDraft((current) => ({ ...current, multiplier: event.target.value }))}
                      className='pr-9 font-mono tabular-nums'
                      aria-invalid={draft.multiplier.length > 0 && parseMultiplier(draft.multiplier) === null}
                    />
                    <span className='text-muted-foreground pointer-events-none absolute top-1/2 right-3 -translate-y-1/2 text-sm'>×</span>
                  </div>
                  <p className='text-muted-foreground text-xs'>最多六位小数；内部会精确换算为整数 ppm。</p>
                </div>
              </FormSection>

              <FormSection
                title='模型组用户'
                description={
                  editor?.isDefault
                    ? '默认模型组中的现有用户不能在这里移除。把用户分配到其他模型组时，系统会自动移动其个人项目。'
                    : '每位选中用户的个人项目会获得这个模型与价格组合；移除后，该用户会自动回到默认模型组。'
                }
              >
                {draft.status === 'DISABLED' && (
                  <CatalogWarning message='停用模型组不能调整成员。重新启用后即可选择用户；本次保存会保留现有成员。' />
                )}
                {!canReadUsers ? (
                  <CatalogWarning
                    message={
                      editor
                        ? '缺少 read_users 权限，无法读取用户目录；保存时会保留当前成员。'
                        : '缺少 read_users 权限，本次将创建一个暂未分配用户的模型组。'
                    }
                  />
                ) : usersQuery.isError ? (
                  <CatalogWarning
                    message={editor ? '用户目录加载失败；保存时会保留当前成员。' : '用户目录加载失败，本次将不分配用户。'}
                    onRetry={() => void usersQuery.refetch()}
                  />
                ) : usersQuery.isLoading ? (
                  <PickerLoading />
                ) : (
                  <UserPicker
                    users={users}
                    values={draft.memberUserIDs}
                    lockedValues={editor?.isDefault ? editor.memberUserIDs.map(entityID) : []}
                    disabled={draft.status === 'DISABLED'}
                    search={userSearch}
                    onSearch={setUserSearch}
                    onChange={(memberUserIDs) => setDraft((current) => ({ ...current, memberUserIDs }))}
                  />
                )}
              </FormSection>

              {mode === 'ENTERPRISE' && editor && <EnterpriseDetails group={editor} dialog />}
            </div>
          </ScrollArea>

          <DialogFooter className='border-t px-6 py-4'>
            <Button variant='outline' onClick={closeEditor} disabled={isSaving}>
              取消
            </Button>
            <Button
              onClick={() => void submit()}
              disabled={
                isSaving ||
                (!editor &&
                  (!canReadModels ||
                    modelsQuery.isLoading ||
                    modelsQuery.isError ||
                    (mode === 'ENTERPRISE' && (routesQuery.isLoading || routesQuery.isError)) ||
                    (canReadUsers && usersQuery.isLoading)))
              }
            >
              {isSaving && <IconLoader2 className='animate-spin' />}
              {isSaving ? '保存中…' : editor ? '保存更改' : '创建模型组'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <ConfirmDialog
        open={archiveTarget !== null}
        onOpenChange={(open) => !open && !archiveGroup.isPending && setArchiveTarget(null)}
        title='归档模型组'
        desc={
          <p>
            确定归档“<span className='text-foreground font-medium'>{archiveTarget?.name}</span>”吗？归档后不能继续编辑，已有商业记录会保留。
          </p>
        }
        confirmText={archiveGroup.isPending ? '归档中…' : '确认归档'}
        cancelBtnText='取消'
        destructive
        isLoading={archiveGroup.isPending}
        handleConfirm={() => void archive()}
      />
    </>
  );
}

function GroupOverview({ groups }: { groups: SimpleGroup[] }) {
  const currentGroups = groups.filter((group) => group.status !== 'ARCHIVED');
  const activeCount = currentGroups.filter((group) => group.status === 'ENABLED').length;
  const assignedUsers = new Set(currentGroups.flatMap((group) => group.memberUserIDs.map(entityID))).size;
  const defaultGroup = currentGroups.find((group) => group.isDefault);

  return (
    <section className='bg-card grid grid-cols-3 divide-x rounded-lg border' aria-label='模型组概览'>
      <OverviewItem label='启用模型组' value={String(activeCount)} />
      <OverviewItem label='已分配用户' value={String(assignedUsers)} />
      <OverviewItem label='默认模型组' value={defaultGroup?.name || '未设置'} text />
    </section>
  );
}

function OverviewItem({ label, value, text = false }: { label: string; value: string; text?: boolean }) {
  return (
    <div className='min-w-0 px-4 py-3'>
      <p className='text-muted-foreground text-xs'>{label}</p>
      <p className={`mt-0.5 truncate font-semibold tabular-nums ${text ? 'text-sm sm:text-base' : 'text-lg'}`} title={value}>
        {value}
      </p>
    </div>
  );
}

function GroupRow({
  group,
  enterprise,
  canWrite,
  archivePending,
  onEdit,
  onArchive,
}: {
  group: SimpleGroup;
  enterprise: boolean;
  canWrite: boolean;
  archivePending: boolean;
  onEdit: () => void;
  onArchive: () => void;
}) {
  const archived = group.status === 'ARCHIVED';
  const status = {
    ENABLED: {
      label: '启用',
      className: 'border-emerald-500/25 bg-emerald-500/10 text-emerald-700 dark:text-emerald-400',
    },
    DISABLED: { label: '停用', className: 'border-border bg-muted text-muted-foreground' },
    ARCHIVED: { label: '已归档', className: 'border-destructive/25 bg-destructive/5 text-destructive' },
  }[group.status];

  return (
    <article className={`p-4 sm:p-5 ${archived ? 'bg-muted/20' : ''}`}>
      <div className='grid gap-4 xl:grid-cols-[minmax(13rem,1fr)_minmax(25rem,auto)_auto] xl:items-center'>
        <div className='min-w-0'>
          <div className='flex flex-wrap items-center gap-2'>
            <h2 className={`truncate font-semibold ${archived ? 'text-muted-foreground' : ''}`}>{group.name}</h2>
            {group.isDefault && <Badge>默认</Badge>}
            <Badge variant='outline' className={status.className}>
              {status.label}
            </Badge>
          </div>
          <p className='text-muted-foreground mt-1 line-clamp-2 text-sm'>{group.description || '暂无说明'}</p>
        </div>

        <BundleEquation group={group} />

        <div className='flex items-center gap-2 xl:justify-end'>
          <Button
            variant='outline'
            size='sm'
            onClick={onEdit}
            disabled={!canWrite || archived}
            title={!canWrite ? '需要 write_groups 权限' : archived ? '已归档模型组不能编辑' : undefined}
          >
            <IconEdit />
            编辑
          </Button>
          <Button
            variant='outline'
            size='icon-sm'
            onClick={onArchive}
            disabled={!canWrite || group.isDefault || archived || archivePending}
            aria-label={`归档 ${group.name}`}
            title={group.isDefault ? '默认模型组不能归档' : archived ? '该模型组已归档' : '归档模型组'}
          >
            {archivePending ? <IconLoader2 className='animate-spin' /> : <IconArchive />}
          </Button>
        </div>
      </div>

      {group.unresolvedMemberCount > 0 && (
        <div className='mt-3 flex items-start gap-2 border-t border-amber-500/20 pt-3 text-xs text-amber-700 dark:text-amber-400'>
          <IconAlertTriangle className='mt-px size-3.5 shrink-0' />有 {group.unresolvedMemberCount}{' '}
          位成员无法映射到唯一的个人项目，请检查这些用户的项目配置。
        </div>
      )}

      {enterprise && <EnterpriseDetails group={group} />}
    </article>
  );
}

function BundleEquation({ group }: { group: SimpleGroup }) {
  const routeLabel = group.routeIDs.length === 0 ? '全部可用上游' : `${group.routeIDs.length} 个上游映射`;
  return (
    <div
      className='bg-muted/35 flex flex-wrap items-center gap-x-2 gap-y-1 rounded-md border px-3 py-2 text-sm'
      aria-label={`${group.modelIDs.length} 个对外模型，${routeLabel}，加 ${formatMultiplier(group.multiplierPpm)} 倍率，加 ${group.memberUserIDs.length} 位用户，组成一个模型组`}
    >
      <EquationValue value={String(group.modelIDs.length)} label='对外模型' />
      <EquationOperator>+</EquationOperator>
      <EquationValue value={group.routeIDs.length === 0 ? '全部' : String(group.routeIDs.length)} label='上游映射' />
      <EquationOperator>+</EquationOperator>
      <EquationValue value={`${formatMultiplier(group.multiplierPpm)}×`} label='倍率' />
      <EquationOperator>+</EquationOperator>
      <EquationValue value={String(group.memberUserIDs.length)} label='用户' />
      <EquationOperator>=</EquationOperator>
      <span className='font-medium whitespace-nowrap'>1 个模型组</span>
    </div>
  );
}

function EquationValue({ value, label }: { value: string; label: string }) {
  return (
    <span className='whitespace-nowrap'>
      <span className='font-mono font-semibold tabular-nums'>{value}</span> <span className='text-muted-foreground text-xs'>{label}</span>
    </span>
  );
}

function EquationOperator({ children }: { children: ReactNode }) {
  return (
    <span className='text-muted-foreground font-mono text-xs' aria-hidden='true'>
      {children}
    </span>
  );
}

function EnterpriseDetails({ group, dialog = false }: { group: SimpleGroup; dialog?: boolean }) {
  return (
    <div
      className={
        dialog
          ? 'bg-muted/25 rounded-md border p-4'
          : 'text-muted-foreground mt-3 flex flex-col gap-2 border-t pt-3 text-xs sm:flex-row sm:items-center sm:justify-between'
      }
    >
      <div>
        {dialog && <p className='text-foreground mb-2 text-sm font-medium'>企业模式映射</p>}
        <div className='flex flex-wrap gap-x-4 gap-y-1 font-mono tabular-nums'>
          <span>Access Plan: {group.accessPlanID}</span>
          <span>Price Tier: {group.priceTierID}</span>
          <span>成员项目: {group.memberProjectIDs.length}</span>
        </div>
      </div>
    </div>
  );
}

function FormSection({ title, description, children }: { title: string; description: string; children: ReactNode }) {
  return (
    <section>
      <div className='mb-3'>
        <h3 className='text-sm font-semibold'>{title}</h3>
        <p className='text-muted-foreground mt-0.5 text-xs leading-5'>{description}</p>
      </div>
      <div className='rounded-md border p-4'>{children}</div>
    </section>
  );
}

function SettingToggle({
  label,
  hint,
  checked,
  disabled,
  onChange,
}: {
  label: string;
  hint: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <div className='flex min-h-16 items-center justify-between gap-2 rounded-md border px-3 py-2'>
      <div className='min-w-0'>
        <Label className='text-sm'>{label}</Label>
        <p className='text-muted-foreground mt-0.5 text-[11px] leading-4'>{hint}</p>
      </div>
      <Switch checked={checked} disabled={disabled} onCheckedChange={onChange} aria-label={label} />
    </div>
  );
}

function PermissionPanel({
  step,
  title,
  hint,
  count,
  unrestricted = false,
  advanced = false,
  children,
}: {
  step: string;
  title: string;
  hint: string;
  count: number;
  unrestricted?: boolean;
  advanced?: boolean;
  children: ReactNode;
}) {
  return (
    <section
      className={`min-w-0 rounded-lg border p-4 ${advanced ? 'border-amber-500/20 bg-amber-500/[0.025]' : 'bg-muted/10'}`}
      aria-label={title}
    >
      <div className='mb-4 flex items-start justify-between gap-4'>
        <div className='flex min-w-0 gap-3'>
          <span
            className={`mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-md border font-mono text-xs font-semibold tabular-nums ${
              advanced ? 'border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300' : 'bg-background text-foreground'
            }`}
            aria-hidden='true'
          >
            {step}
          </span>
          <div className='min-w-0'>
            <h4 className='text-sm font-semibold'>{title}</h4>
            <p className='text-muted-foreground mt-1 text-xs leading-5'>{hint}</p>
          </div>
        </div>
        <Badge variant='outline' className='shrink-0 font-mono tabular-nums'>
          {unrestricted ? '不限制' : `已选 ${count}`}
        </Badge>
      </div>
      {children}
    </section>
  );
}

function ModelPicker({
  models,
  values,
  search,
  onSearch,
  onChange,
}: {
  models: GroupModelOption[];
  values: string[];
  search: string;
  onSearch: (value: string) => void;
  onChange: (values: string[]) => void;
}) {
  const query = search.trim().toLocaleLowerCase();
  const options = models.filter((model) => `${model.name} ${model.modelID}`.toLocaleLowerCase().includes(query));
  const available = new Set(models.map((model) => entityID(model.id)));
  const hiddenSelected = values.filter((id) => !available.has(entityID(id))).length;
  const normalizedValues = values.map(entityID);
  const visibleIDs = options.map((model) => entityID(model.id));
  const allVisibleSelected = visibleIDs.length > 0 && visibleIDs.every((id) => normalizedValues.includes(id));

  return (
    <PickerFrame
      icon={<IconBox />}
      search={search}
      onSearch={onSearch}
      placeholder='搜索对外模型名称或 ID'
      selectedCount={values.length}
      hiddenSelected={hiddenSelected}
      actions={
        <>
          <Button
            type='button'
            variant='ghost'
            size='sm'
            className='h-10 px-2.5'
            disabled={visibleIDs.length === 0 || allVisibleSelected}
            onClick={() => onChange([...new Set([...normalizedValues, ...visibleIDs])])}
          >
            全选可见
          </Button>
          <Button
            type='button'
            variant='ghost'
            size='sm'
            className='h-10 px-2.5'
            disabled={!visibleIDs.some((id) => normalizedValues.includes(id))}
            onClick={() => onChange(normalizedValues.filter((id) => !visibleIDs.includes(id)))}
          >
            清除可见
          </Button>
        </>
      }
    >
      {options.length === 0 ? (
        <PickerEmpty>{models.length === 0 ? '当前没有可用的对外模型。' : '没有匹配的对外模型。'}</PickerEmpty>
      ) : (
        options.map((model) => {
          const id = entityID(model.id);
          return (
            <PickerRow
              key={model.id}
              checked={normalizedValues.includes(id)}
              onChange={(checked) => onChange(toggle(normalizedValues, id, checked))}
            >
              <span className='font-medium break-words'>{model.name}</span>
              <span className='text-muted-foreground mt-0.5 font-mono text-xs break-all'>{model.modelID}</span>
            </PickerRow>
          );
        })
      )}
    </PickerFrame>
  );
}

function RoutePicker({
  routes,
  values,
  search,
  onSearch,
  onChange,
}: {
  routes: GroupRouteOption[];
  values: string[];
  search: string;
  onSearch: (value: string) => void;
  onChange: (values: string[]) => void;
}) {
  const query = search.trim().toLocaleLowerCase();
  const options = routes.filter((route) =>
    `${route.publicModelKey} ${route.deploymentName} ${route.channelName} ${route.upstreamModelID}`.toLocaleLowerCase().includes(query)
  );
  const available = new Set(routes.map((route) => entityID(route.id)));
  const hiddenSelected = values.filter((id) => !available.has(entityID(id))).length;
  const normalizedValues = values.map(entityID);

  return (
    <PickerFrame
      icon={<IconRoute />}
      search={search}
      onSearch={onSearch}
      placeholder='搜索对外模型、上游模型或上游渠道'
      selectedCount={values.length}
      hiddenSelected={hiddenSelected}
      actions={
        values.length > 0 ? (
          <Button type='button' variant='ghost' size='sm' className='h-10 px-2.5' onClick={() => onChange([])}>
            取消路由限制
          </Button>
        ) : undefined
      }
    >
      {options.length === 0 ? (
        <PickerEmpty>{routes.length === 0 ? '当前没有可配置的上游映射。' : '没有匹配的上游映射。'}</PickerEmpty>
      ) : (
        options.map((route) => {
          const id = entityID(route.id);
          const enabled = route.status === 'ENABLED';
          const checked = normalizedValues.includes(id);
          return (
            <PickerRow
              key={route.id}
              checked={checked}
              disabled={!enabled && !checked}
              title={!enabled ? (checked ? '该上游映射已停用，请取消选择后再保存' : '已停用的上游映射不可授权') : undefined}
              onChange={(next) => onChange(toggle(normalizedValues, id, next))}
            >
              <span className='flex min-w-0 flex-wrap items-center gap-1.5'>
                <span className='font-medium break-words'>{route.channelName}</span>
                {!enabled && (
                  <Badge variant='outline' className='text-muted-foreground shrink-0 text-[10px]'>
                    停用
                  </Badge>
                )}
              </span>
              <span className='mt-1 grid gap-1 text-xs sm:grid-cols-[auto_minmax(0,1fr)] sm:gap-x-2'>
                <span className='text-muted-foreground'>上游模型</span>
                <span className='font-mono break-all'>{route.upstreamModelID}</span>
                <span className='text-muted-foreground'>对应对外模型</span>
                <span className='font-mono break-all'>{route.publicModelKey}</span>
              </span>
            </PickerRow>
          );
        })
      )}
    </PickerFrame>
  );
}

function UserPicker({
  users,
  values,
  lockedValues,
  disabled,
  search,
  onSearch,
  onChange,
}: {
  users: GroupUserOption[];
  values: string[];
  lockedValues: string[];
  disabled: boolean;
  search: string;
  onSearch: (value: string) => void;
  onChange: (values: string[]) => void;
}) {
  const query = search.trim().toLocaleLowerCase();
  const options = users.filter((user) => `${user.firstName} ${user.lastName} ${user.email}`.toLocaleLowerCase().includes(query));
  const available = new Set(users.map((user) => entityID(user.id)));
  const hiddenSelected = values.filter((id) => !available.has(entityID(id))).length;
  const locked = new Set(lockedValues.map(entityID));

  return (
    <PickerFrame
      icon={<IconUsers />}
      search={search}
      onSearch={onSearch}
      placeholder='搜索姓名或邮箱'
      selectedCount={values.length}
      hiddenSelected={hiddenSelected}
    >
      {options.length === 0 ? (
        <PickerEmpty>{users.length === 0 ? '当前没有可分配的用户。' : '没有匹配的用户。'}</PickerEmpty>
      ) : (
        options.map((user) => {
          const id = entityID(user.id);
          const name = `${user.firstName || ''} ${user.lastName || ''}`.trim();
          const isLocked = locked.has(id);
          return (
            <PickerRow
              key={user.id}
              checked={values.map(entityID).includes(id)}
              disabled={disabled || isLocked}
              title={isLocked ? '默认模型组中的现有用户不能直接移除' : undefined}
              onChange={(checked) => onChange(toggle(values.map(entityID), id, checked))}
            >
              <span className='truncate'>{name || user.email}</span>
              <span className='text-muted-foreground truncate text-xs'>{name ? user.email : `用户 ID ${id}`}</span>
            </PickerRow>
          );
        })
      )}
    </PickerFrame>
  );
}

function PickerFrame({
  icon,
  search,
  onSearch,
  placeholder,
  selectedCount,
  hiddenSelected,
  actions,
  children,
}: {
  icon: ReactNode;
  search: string;
  onSearch: (value: string) => void;
  placeholder: string;
  selectedCount: number;
  hiddenSelected: number;
  actions?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className='overflow-hidden rounded-md border'>
      <div className='bg-muted/20 flex flex-col gap-2 border-b p-2 sm:flex-row sm:items-center'>
        <div className='relative min-w-0 flex-1'>
          <IconSearch className='text-muted-foreground pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2' />
          <Input
            value={search}
            onChange={(event) => onSearch(event.target.value)}
            placeholder={placeholder}
            className='h-10 pl-8'
            aria-label={placeholder}
          />
        </div>
        <div className='text-muted-foreground flex min-h-10 shrink-0 items-center gap-1.5 px-1 text-xs tabular-nums'>
          <span className='size-4'>{icon}</span>
          已选择 {selectedCount}
        </div>
        {actions && <div className='flex shrink-0 items-center gap-1'>{actions}</div>}
      </div>
      <div className='bg-border grid max-h-80 grid-cols-1 gap-px overflow-y-auto'>{children}</div>
      {hiddenSelected > 0 && (
        <p className='text-muted-foreground border-t px-3 py-2 text-xs'>另有 {hiddenSelected} 项不在当前目录中，保存时仍会保留。</p>
      )}
    </div>
  );
}

function PickerRow({
  checked,
  disabled,
  title,
  onChange,
  children,
}: {
  checked: boolean;
  disabled?: boolean;
  title?: string;
  onChange: (checked: boolean) => void;
  children: ReactNode;
}) {
  return (
    <Label
      className={`bg-background hover:bg-muted/60 focus-within:ring-ring flex min-h-14 min-w-0 cursor-pointer items-center gap-3 px-3 py-2.5 font-normal transition-colors focus-within:ring-2 focus-within:ring-inset ${
        checked ? 'bg-primary/5 ring-primary/30 hover:bg-primary/10 ring-1 ring-inset' : ''
      } ${disabled ? 'cursor-not-allowed opacity-60' : ''}`}
      title={title}
    >
      <Checkbox checked={checked} disabled={disabled} onCheckedChange={(value) => onChange(!!value)} />
      <span className='grid min-w-0 leading-4'>{children}</span>
    </Label>
  );
}

function PickerEmpty({ children }: { children: ReactNode }) {
  return <p className='text-muted-foreground bg-background col-span-full px-3 py-8 text-center text-sm'>{children}</p>;
}

function CatalogWarning({ message, onRetry }: { message: string; onRetry?: () => void }) {
  return (
    <div className='flex items-start gap-2 rounded-md border border-amber-500/25 bg-amber-500/5 px-3 py-2.5 text-sm text-amber-800 dark:text-amber-300'>
      <IconAlertTriangle className='mt-0.5 size-4 shrink-0' />
      <span className='min-w-0 flex-1'>{message}</span>
      {onRetry && (
        <Button variant='ghost' size='sm' className='-my-1 h-7' onClick={onRetry}>
          <IconRefresh />
          重试
        </Button>
      )}
    </div>
  );
}

function PickerLoading() {
  return (
    <div className='space-y-2' aria-label='正在加载目录'>
      <Skeleton className='h-8 w-full' />
      <div className='grid gap-2 sm:grid-cols-2'>
        <Skeleton className='h-12' />
        <Skeleton className='h-12' />
        <Skeleton className='h-12' />
        <Skeleton className='h-12' />
      </div>
    </div>
  );
}

function GroupsLoading() {
  return (
    <div className='space-y-4' aria-label='正在加载模型组'>
      <Skeleton className='h-[70px] w-full' />
      <div className='overflow-hidden rounded-lg border'>
        {[0, 1, 2].map((item) => (
          <div key={item} className='grid gap-4 border-b p-5 last:border-b-0 lg:grid-cols-[1fr_26rem]'>
            <div className='space-y-2'>
              <Skeleton className='h-5 w-40' />
              <Skeleton className='h-4 w-64 max-w-full' />
            </div>
            <Skeleton className='h-12 w-full' />
          </div>
        ))}
      </div>
    </div>
  );
}

function GroupsError({ message, onRetry }: { message: string; onRetry: () => void }) {
  return (
    <section className='border-destructive/30 bg-destructive/5 rounded-lg border px-5 py-8 text-center' role='alert'>
      <IconAlertTriangle className='text-destructive mx-auto size-6' />
      <h2 className='mt-3 font-semibold'>模型组加载失败</h2>
      <p className='text-muted-foreground mx-auto mt-1 max-w-xl text-sm'>{message}</p>
      <Button variant='outline' size='sm' className='mt-4' onClick={onRetry}>
        <IconRefresh />
        重新加载
      </Button>
    </section>
  );
}

function EmptyGroups({ canWrite, onCreate }: { canWrite: boolean; onCreate: () => void }) {
  return (
    <section className='bg-card rounded-lg border border-dashed px-6 py-14 text-center'>
      <span className='bg-muted text-muted-foreground mx-auto flex size-11 items-center justify-center rounded-md'>
        <IconUsersGroup className='size-5' />
      </span>
      <h2 className='mt-4 font-semibold'>还没有模型组</h2>
      <p className='text-muted-foreground mx-auto mt-1 max-w-md text-sm'>创建第一个模型组，为用户一次配置可用模型和零售倍率。</p>
      {canWrite && (
        <Button size='sm' className='mt-4' onClick={onCreate}>
          <IconPlus />
          新建模型组
        </Button>
      )}
    </section>
  );
}
