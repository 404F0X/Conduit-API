import { useQuery } from '@tanstack/react-query';
import { graphqlRequest } from '@/gql/graphql';
import { useTranslation } from 'react-i18next';
import { useSelectedProjectId } from '@/stores/projectStore';
import { useErrorHandler } from '@/hooks/use-error-handler';
import type { ApiKeyProfile, ApiKeyProfiles } from '@/features/apikeys/data/schema';

/**
 * 简单模式令牌列表。和完整模式共用同一套接口（apiKeys / createAPIKey / updateAPIKeyProfiles），
 * 只是列表里顺带取回第一个配置（可用模型、到期时间、金额额度），
 * 这样表格的“可用模型”“到期”“额度”三列不用每行再查一次。
 */

export type SimpleTokenStatus = 'enabled' | 'disabled' | 'archived';

export interface SimpleToken {
  id: string;
  createdAt: string;
  name: string;
  key: string;
  status: SimpleTokenStatus;
  modelGroupID?: string | null;
  profiles: ApiKeyProfiles | null;
}

interface SimpleTokenConnection {
  edges: Array<{ node: SimpleToken }>;
  pageInfo: { hasNextPage: boolean; endCursor?: string | null };
  totalCount: number;
}

const SIMPLE_TOKENS_QUERY = `
  query SimpleTokens($first: Int, $after: Cursor, $orderBy: APIKeyOrder, $where: APIKeyWhereInput) {
    apiKeys(first: $first, after: $after, orderBy: $orderBy, where: $where) {
      edges {
        node {
          id
          createdAt
          name
          key
          status
          modelGroupID
          profiles {
            activeProfile
            profiles {
              name
              modelIDs
              validFrom
              loadBalanceStrategy
              maxConcurrentRequests
              channelTagsMatchMode
              validUntil
              channelIDs
              channelTags
              modelMappings { from to }
              quota { requests totalTokens cost period { type pastDuration { value unit } calendarDuration { unit } } }
            }
          }
        }
      }
      pageInfo { hasNextPage endCursor }
      totalCount
    }
  }
`;

export function useSimpleTokens(variables: { first: number; after?: string; nameContainsFold?: string }) {
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();
  const projectId = useSelectedProjectId();

  return useQuery({
    queryKey: ['apiKeys', 'simple', variables, projectId],
    queryFn: async () => {
      try {
        const data = await graphqlRequest<{ apiKeys: SimpleTokenConnection }>(
          SIMPLE_TOKENS_QUERY,
          {
            first: variables.first,
            after: variables.after,
            orderBy: { field: 'CREATED_AT', direction: 'DESC' },
            where: {
              projectID: projectId,
              typeIn: ['user'],
              statusIn: ['enabled', 'disabled'],
              ...(variables.nameContainsFold ? { nameContainsFold: variables.nameContainsFold } : {}),
            },
          },
          projectId ? { 'X-Project-ID': projectId } : undefined
        );
        return data.apiKeys;
      } catch (error) {
        handleError(error, t('common.errors.internalServerError'));
        throw error;
      }
    },
    enabled: !!projectId,
  });
}

/** 令牌的“当前配置”。简单模式只认一个配置。 */
export function activeProfileOf(token: Pick<SimpleToken, 'profiles'>) {
  const list = token.profiles?.profiles ?? [];
  return list.find((p) => p.name === token.profiles?.activeProfile) ?? list[0] ?? null;
}

/** 有多个配置，或用了渠道限定、模型映射、请求数 / Token 数额度：这些只能在完整模式里改。 */
export function hasAdvancedSettings(token: Pick<SimpleToken, 'profiles'>): boolean {
  const list = token.profiles?.profiles ?? [];
  if (list.length > 1) return true;
  const profile = list[0];
  if (!profile) return false;
  return (
    !!profile.validFrom ||
    (profile.maxConcurrentRequests != null && profile.maxConcurrentRequests !== 0) ||
    !!profile.loadBalanceStrategy ||
    (profile.channelTagsMatchMode != null && profile.channelTagsMatchMode !== 'any') ||
    (profile.channelIDs?.length ?? 0) > 0 ||
    (profile.channelTags?.length ?? 0) > 0 ||
    (profile.modelMappings?.length ?? 0) > 0 ||
    profile.quota?.requests != null ||
    profile.quota?.totalTokens != null ||
    (profile.quota?.period?.type != null && profile.quota.period.type !== 'all_time')
  );
}

export interface SimpleTokenForm {
  name: string;
  modelGroupID?: string;
  expiryChanged?: boolean;
  /** 额度（站点展示单位），null = 不限 */
  quota: number | null;
  /** 到期时间，null = 永久 */
  expiresAt: Date | null;
  /** 可用模型，空数组 = 全部 */
  modelIds: string[];
}

/** New keys start with a plain profile; edits merge the existing value below. */
export function buildSimpleProfiles(form: SimpleTokenForm, profileName: string): ApiKeyProfiles {
  const profile: ApiKeyProfile = {
    name: profileName,
    modelMappings: [],
    channelIDs: [],
    channelTags: [],
    channelTagsMatchMode: 'any',
    modelIDs: form.modelIds,
    validFrom: null,
    validUntil: form.expiresAt ? form.expiresAt.toISOString() : null,
    quota:
      form.quota != null
        ? { requests: null, totalTokens: null, cost: form.quota, period: { type: 'all_time', pastDuration: null, calendarDuration: null } }
        : null,
  };
  return { activeProfile: profile.name, profiles: [profile] };
}

/** Rename and profile replacement are separate server operations. */
export function planSimpleTokenUpdate(token: SimpleToken, form: SimpleTokenForm, profileName: string) {
  const name = form.name !== token.name ? { name: form.name } : undefined;
  if (hasAdvancedSettings(token)) return { name, profiles: undefined };
  const original = activeProfileOf(token);
  const oldCost = original?.quota?.cost ?? null;
  const quotaChanged = form.quota !== oldCost;
  const modelsChanged = JSON.stringify(form.modelIds) !== JSON.stringify(original?.modelIDs ?? []);
  if (!quotaChanged && !modelsChanged && !form.expiryChanged) return { name, profiles: undefined };
  if (!original || !token.profiles) return { name, profiles: buildSimpleProfiles(form, profileName) };
  const updated: ApiKeyProfile = {
    ...original,
    ...(modelsChanged ? { modelIDs: [...form.modelIds] } : {}),
    ...(form.expiryChanged ? { validUntil: form.expiresAt?.toISOString() ?? null } : {}),
    ...(quotaChanged
      ? {
          quota: {
            ...(original.quota ?? { period: { type: 'all_time' as const } }),
            cost: form.quota,
          },
        }
      : {}),
  };
  return {
    name,
    profiles: {
      ...token.profiles,
      profiles: token.profiles.profiles.map((profile) => (profile === original ? updated : profile)),
    },
  };
}

export function buildSimpleTokenCreation(
  form: SimpleTokenForm,
  profileName: string,
  groups: { id: string }[],
  catalog: { configured: boolean; modelGroupID: string | null; models: { id: string }[] } | undefined
) {
  if (
    !form.modelGroupID ||
    !groups.some((group) => group.id === form.modelGroupID) ||
    !catalog?.configured ||
    catalog.modelGroupID !== form.modelGroupID
  ) {
    throw new Error('Select an authorized, configured model group.');
  }
  if (form.modelIds.some((id) => !catalog.models.some((model) => model.id === id))) {
    throw new Error('Selected models are unavailable in this model group.');
  }
  return { name: form.name, type: 'user' as const, modelGroupID: form.modelGroupID, profiles: buildSimpleProfiles(form, profileName) };
}

/** The date control displays local dates, while untouched expiry stays verbatim in profiles. */
export function localExpiryDate(date: Date) {
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`;
}
