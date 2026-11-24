import { useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { graphqlRequest } from '@/gql/graphql';
import { useTranslation } from 'react-i18next';
import { usePermissions } from '@/hooks/usePermissions';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Main } from '@/components/layout/main';
import { useSimpleGroupModelsCatalog, useSimpleGroupUsersCatalog } from './data';
import ProjectRulesPanel from './project-rules-panel';
import { rechargeMultiplierToPPM, rechargePPMToMultiplier } from './recharge-multiplier';

type Status = 'ENABLED' | 'DISABLED' | 'ARCHIVED';
type ModelGroup = { id: string; name: string; description: string | null; status: Status; revision: number; modelIDs: string[] };
type UserGroup = {
  id: string;
  name: string;
  description: string | null;
  status: Status;
  memberUserIDs: string[];
  modelGroupIDs: string[];
  rechargeMultiplierPPM: string;
  rechargeRevision: string;
};
const query = `query IndependentGroups { modelGroups { id name description status revision modelIDs } userGroups { id name description status memberUserIDs modelGroupIDs rechargeMultiplierPPM rechargeRevision } }`;
export default function GroupsPage() {
  const { t } = useTranslation();
  const [tab, setTab] = useState<'users' | 'models' | 'projects'>('users');
  const { hasSystemScope } = usePermissions();
  const canWrite = hasSystemScope('write_users');
  const client = useQueryClient();
  const catalog = useQuery({
    queryKey: ['independent-groups'],
    queryFn: () => graphqlRequest<{ modelGroups: ModelGroup[]; userGroups: UserGroup[] }>(query),
  });
  const models = useSimpleGroupModelsCatalog(canWrite);
  const users = useSimpleGroupUsersCatalog(canWrite);
  const [editing, setEditing] = useState<string | undefined>();
  const [name, setName] = useState('');
  const [description, setDescription] = useState('');
  const [status, setStatus] = useState<Status>('ENABLED');
  const [selected, setSelected] = useState<string[]>([]);
  const [members, setMembers] = useState<string[]>([]);
  const [error, setError] = useState('');
  const [rechargeMultiplier, setRechargeMultiplier] = useState('1');
  const [rechargeRevision, setRechargeRevision] = useState<string>();
  const [open, setOpen] = useState(false);
  function reset() {
    setEditing(undefined);
    setName('');
    setDescription('');
    setStatus('ENABLED');
    setSelected([]);
    setMembers([]);
    setRechargeMultiplier('1');
    setRechargeRevision(undefined);
    setError('');
    setOpen(false);
  }
  const save = useMutation({
    mutationFn: async () => {
      const model = tab === 'models';
      const ppm = rechargeMultiplierToPPM(rechargeMultiplier);
      if (!model && (!ppm || (editing && !rechargeRevision))) throw new Error(t('users.groups.recharge.invalid'));
      const input = {
        id: editing,
        name,
        description: description || null,
        status,
        ...(model
          ? { modelIDs: selected }
          : {
              memberUserIDs: members,
              modelGroupIDs: selected,
              rechargeMultiplierPPM: ppm,
              ...(editing ? { expectedRechargeRevision: rechargeRevision } : {}),
            }),
      };
      await graphqlRequest(
        `mutation SaveGroup($input:${model ? 'SaveModelGroupInput' : 'SaveUserGroupInput'}!){${model ? 'saveModelGroup' : 'saveUserGroup'}(input:$input){id}}`,
        { input }
      );
    },
    onSuccess: async () => {
      reset();
      await client.invalidateQueries({ queryKey: ['independent-groups'] });
      await client.invalidateQueries({ queryKey: ['api-key-assignable-model-groups'] });
    },
    onError: (e) => setError(e instanceof Error ? e.message : String(e)),
  });
  function toggle(id: string, list: string[], set: (v: string[]) => void) {
    set(list.includes(id) ? list.filter((x) => x !== id) : [...list, id]);
  }
  return (
    <>
      <Main>
        <h1 className='text-2xl font-semibold'>分组管理</h1>
        <div className='my-4 flex gap-2' role='tablist' aria-label='分组类型'>
          {(
            [
              ['users', '用户组'],
              ['models', '模型组'],
              ['projects', 'Project 规则'],
            ] as const
          ).map(([id, label]) => (
            <Button
              key={id}
              role='tab'
              aria-selected={tab === id}
              variant={tab === id ? 'default' : 'outline'}
              onClick={() => {
                reset();
                setTab(id);
              }}
            >
              {label}
            </Button>
          ))}
        </div>
        {tab !== 'projects' && (
          <section className='space-y-4'>
            <p className='text-muted-foreground'>
              {tab === 'users'
                ? t('users.groups.recharge.description')
                : '模型组整理公开模型，Key 固定属于一个模型组；可用范围仍受 Project 和 Key 规则限制。'}
            </p>
            {catalog.isError && <p role='alert'>分组加载失败，请重试。</p>}
            {catalog.isPending && <p>加载中…</p>}
            {canWrite && (
              <Button
                onClick={() => {
                  reset();
                  setOpen(true);
                }}
              >
                新建{tab === 'models' ? '模型组' : '用户组'}
              </Button>
            )}
            <div className='grid gap-3'>
              {(tab === 'models' ? catalog.data?.modelGroups : catalog.data?.userGroups)?.map((group) => (
                <article className='rounded border p-4' key={group.id}>
                  <div className='flex items-center justify-between gap-3'>
                    <div>
                      <h2 className='font-semibold'>{group.name}</h2>
                      <p className='text-muted-foreground text-sm'>
                        {group.description} · {group.status}
                      </p>
                      <p className='text-sm'>
                        {'modelIDs' in group
                          ? `${group.modelIDs.length} 个模型`
                          : `${group.memberUserIDs.length} 位用户 / ${group.modelGroupIDs.length} 个模型组`}
                      </p>
                      {'rechargeMultiplierPPM' in group && (
                        <p className='text-sm'>
                          {t('users.groups.recharge.summary', {
                            multiplier: rechargePPMToMultiplier(group.rechargeMultiplierPPM) ?? '—',
                            ppm: group.rechargeMultiplierPPM,
                          })}
                        </p>
                      )}
                    </div>
                    {canWrite && (
                      <Button
                        variant='outline'
                        onClick={() => {
                          setEditing(group.id);
                          setName(group.name);
                          setDescription(group.description || '');
                          setStatus(group.status);
                          setSelected('modelIDs' in group ? group.modelIDs : group.modelGroupIDs);
                          setMembers('memberUserIDs' in group ? group.memberUserIDs : []);
                          setRechargeMultiplier(
                            'rechargeMultiplierPPM' in group ? (rechargePPMToMultiplier(group.rechargeMultiplierPPM) ?? '') : '1'
                          );
                          setRechargeRevision('rechargeRevision' in group ? group.rechargeRevision : undefined);
                          setError('');
                          setOpen(true);
                        }}
                      >
                        编辑
                      </Button>
                    )}
                  </div>
                </article>
              ))}
            </div>
            {open && (
              <form
                className='space-y-4 rounded border p-4'
                onSubmit={(e) => {
                  e.preventDefault();
                  save.mutate();
                }}
              >
                <h2 className='font-semibold'>
                  {editing ? '编辑' : '新建'}
                  {tab === 'models' ? '模型组' : '用户组'}
                </h2>
                <label className='block'>
                  名称
                  <Input required value={name} onChange={(e) => setName(e.target.value)} />
                </label>
                <label className='block'>
                  说明
                  <Input value={description} onChange={(e) => setDescription(e.target.value)} />
                </label>
                <label className='block'>
                  状态{' '}
                  <select className='rounded border p-2' value={status} onChange={(e) => setStatus(e.target.value as Status)}>
                    <option value='ENABLED'>启用</option>
                    <option value='DISABLED'>停用</option>
                    <option value='ARCHIVED'>归档</option>
                  </select>
                </label>
                {tab === 'users' && (
                  <div className='space-y-2'>
                    <label className='grid gap-1.5 text-sm'>
                      {t('users.groups.recharge.label')}
                      <Input
                        inputMode='decimal'
                        value={rechargeMultiplier}
                        onChange={(event) => setRechargeMultiplier(event.target.value)}
                      />
                    </label>
                    <p className='text-muted-foreground text-xs'>{t('users.groups.recharge.hint')}</p>
                    {!rechargeMultiplierToPPM(rechargeMultiplier) && (
                      <p role='alert' className='text-destructive text-sm'>
                        {t('users.groups.recharge.invalid')}
                      </p>
                    )}
                  </div>
                )}
                <fieldset className='space-y-2'>
                  <legend>{tab === 'models' ? '成员模型' : '授权模型组'}</legend>
                  {(tab === 'models'
                    ? (models.data?.models.edges || []).map((e) => ({ id: e.node.modelID, name: e.node.name || e.node.modelID }))
                    : catalog.data?.modelGroups || []
                  ).map((item) => (
                    <label className='mr-4 inline-flex gap-2' key={item.id}>
                      <input type='checkbox' checked={selected.includes(item.id)} onChange={() => toggle(item.id, selected, setSelected)} />
                      {item.name}
                    </label>
                  ))}
                </fieldset>
                {tab === 'users' && (
                  <fieldset>
                    <legend>成员用户（已有其他用户组时需先显式移出）</legend>
                    {users.data?.users.edges.map(({ node: user }) => (
                      <label className='mr-4 inline-flex gap-2' key={user.id}>
                        <input type='checkbox' checked={members.includes(user.id)} onChange={() => toggle(user.id, members, setMembers)} />
                        {user.email}
                      </label>
                    ))}
                  </fieldset>
                )}
                {error && (
                  <p role='alert' className='text-destructive'>
                    {error}
                  </p>
                )}
                <div className='flex gap-2'>
                  <Button
                    disabled={save.isPending || !name.trim() || (tab === 'users' && !rechargeMultiplierToPPM(rechargeMultiplier))}
                    type='submit'
                  >
                    保存
                  </Button>
                  <Button type='button' variant='outline' onClick={reset}>
                    取消
                  </Button>
                </div>
              </form>
            )}
          </section>
        )}
      </Main>
      {tab === 'projects' && <ProjectRulesPanel />}
    </>
  );
}
