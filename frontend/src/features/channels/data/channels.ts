import { z } from 'zod';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { graphqlRequest } from '@/gql/graphql';
import { pageInfoSchema } from '@/gql/pagination';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { useErrorHandler } from '@/hooks/use-error-handler';
import {
  Channel,
  ChannelConnection,
  ChannelSummaryConnection,
  CreateChannelInput,
  UpdateChannelInput,
  channelConnectionSchema,
  channelSchema,
  channelEndpointsResponseSchema,
  BulkImportChannelsInput,
  BulkImportChannelsResult,
  bulkImportChannelsResultSchema,
  BulkUpdateChannelOrderingInput,
  BulkUpdateChannelOrderingResult,
  bulkUpdateChannelOrderingResultSchema,
  channelSummaryConnectionSchema,
  ChannelSettings,
  ChannelPolicies,
  ChannelModelPrice,
  SaveChannelModelPriceInput,
  channelModelPriceSchema,
  TestChannelAPIKeysPayload,
  testChannelAPIKeysPayloadSchema,
  TestAPIKeyResult,
  testAPIKeyResultSchema,
  ChannelQuotaProbeResult,
  channelQuotaProbeResultSchema,
  NewApiPricingProbeResult,
  newApiPricingProbeResultSchema,
} from './schema';

function localizeChannelTestError(error: string | null | undefined, invalidKeyMessage: string) {
  if (!error) return error;

  const normalized = error.toLowerCase();
  if (
    normalized.includes('not a usable openai api key') ||
    normalized.includes('placeholder/short key') ||
    normalized.includes('placeholder or short keys')
  ) {
    return invalidKeyMessage;
  }
  return error;
}

const QUERY_CHANNEL_NAMES_QUERY = `
  query QueryChannelNames($input: QueryChannelInput!) {
    queryChannels(input: $input) {
      edges {
        node {
          name
        }
        cursor
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
  }
`;

const channelNamesConnectionSchema = z.object({
  edges: z.array(
    z.object({
      node: z.object({
        name: z.string(),
      }),
      cursor: z.string(),
    })
  ),
  pageInfo: pageInfoSchema.pick({
    hasNextPage: true,
    endCursor: true,
  }),
});

const CREATE_CHANNEL_MUTATION = `
  mutation CreateChannel($input: CreateChannelInput!) {
    createChannel(input: $input) {
      id
      type
      createdAt
      updatedAt
      baseURL
      websiteURL
      quotaCurrency
      actualQuotaUsed
      quotaRemaining
      name
      status
      policies {
        stream
      }
      supportedModels
      autoSyncSupportedModels
      autoSyncModelPattern
      manualModels
      tags
      defaultTestModel
      settings {
        managementAdapter
        billingCurrency
        rechargeMultiplier
        extraModelPrefix
        modelMappings {
          from
          to
        }
        autoTrimedModelPrefixes
        hideOriginalModels
        hideMappedModels
        lowercaseModelId
        proxy {
          type
          url
          username
          password
        }
        transformOptions {
          forceArrayInstructions
          forceArrayInputs
          replaceDeveloperRoleWithSystem
        }
        passThroughUserAgent
        passThroughBody
        retryableStatusCodes
        retryableErrorPatterns {
          pattern
          regex
        }
        autoModelMappingRules {
          pattern
          publicModelIdTemplate
          createDraft
          developerTemplate
          nameTemplate
          groupTemplate
          modelType
        }
        errorResponseRewriteRules {
          statusCodes
          bodyPattern
          httpStatus
          message
          errorType
          code
          body
        }
      }
      orderingWeight
      remark
      defaultEndpoints {
        apiFormat
        path
        baseURL
        transport
      }
      endpoints {
        apiFormat
        path
        baseURL
        transport
      }
    }
  }
`;

const DUPLICATE_CHANNEL_MUTATION = `
  mutation DuplicateChannel($sourceID: ID!, $input: CreateChannelInput!) {
    duplicateChannel(sourceID: $sourceID, input: $input) {
      id
      type
      createdAt
      updatedAt
      baseURL
      websiteURL
      quotaCurrency
      actualQuotaUsed
      quotaRemaining
      name
      status
      policies {
        stream
      }
      supportedModels
      autoSyncSupportedModels
      autoSyncModelPattern
      manualModels
      tags
      defaultTestModel
      settings {
        managementAdapter
        billingCurrency
        rechargeMultiplier
        extraModelPrefix
        modelMappings {
          from
          to
        }
        autoTrimedModelPrefixes
        hideOriginalModels
        hideMappedModels
        lowercaseModelId
        proxy {
          type
          url
          username
          password
        }
        transformOptions {
          forceArrayInstructions
          forceArrayInputs
          replaceDeveloperRoleWithSystem
        }
        passThroughUserAgent
        passThroughBody
        retryableStatusCodes
        retryableErrorPatterns {
          pattern
          regex
        }
        autoModelMappingRules {
          pattern
          publicModelIdTemplate
          createDraft
          developerTemplate
          nameTemplate
          groupTemplate
          modelType
        }
        errorResponseRewriteRules {
          statusCodes
          bodyPattern
          httpStatus
          message
          errorType
          code
          body
        }
      }
      orderingWeight
      remark
      defaultEndpoints {
        apiFormat
        path
        baseURL
        transport
      }
      endpoints {
        apiFormat
        path
        baseURL
        transport
      }
    }
  }
`;

const BULK_CREATE_CHANNELS_MUTATION = `
  mutation BulkCreateChannels($input: BulkCreateChannelsInput!) {
    bulkCreateChannels(input: $input) {
      id
      type
      createdAt
      updatedAt
      baseURL
      websiteURL
      quotaCurrency
      actualQuotaUsed
      quotaRemaining
      name
      status
      policies {
        stream
      }
      supportedModels
      autoSyncSupportedModels
      autoSyncModelPattern
      manualModels
      tags
      defaultTestModel
      settings {
        managementAdapter
        billingCurrency
        rechargeMultiplier
        extraModelPrefix
        modelMappings {
          from
          to
        }
        autoTrimedModelPrefixes
        hideOriginalModels
        hideMappedModels
        lowercaseModelId
        proxy {
          type
          url
          username
          password
        }
        transformOptions {
          forceArrayInstructions
          forceArrayInputs
          replaceDeveloperRoleWithSystem
        }
        passThroughUserAgent
        passThroughBody
        retryableStatusCodes
        retryableErrorPatterns {
          pattern
          regex
        }
        autoModelMappingRules {
          pattern
          publicModelIdTemplate
          createDraft
          developerTemplate
          nameTemplate
          groupTemplate
          modelType
        }
        errorResponseRewriteRules {
          statusCodes
          bodyPattern
          httpStatus
          message
          errorType
          code
          body
        }
      }
      orderingWeight
      remark
      defaultEndpoints {
        apiFormat
        path
        baseURL
        transport
      }
      endpoints {
        apiFormat
        path
        baseURL
        transport
      }
    }
  }
`;

const UPDATE_CHANNEL_MUTATION = `
  mutation UpdateChannel($id: ID!, $input: UpdateChannelInput!) {
    updateChannel(id: $id, input: $input) {
      id
      type
      createdAt
      updatedAt
      baseURL
      websiteURL
      quotaCurrency
      actualQuotaUsed
      quotaRemaining
      name
      status
      policies {
        stream
      }
      supportedModels
      autoSyncSupportedModels
      autoSyncModelPattern
      manualModels
      tags
      defaultTestModel
      settings {
        managementAdapter
        billingCurrency
        rechargeMultiplier
        extraModelPrefix
        modelMappings {
          from
          to
        }
        autoTrimedModelPrefixes
        hideOriginalModels
        hideMappedModels
        lowercaseModelId
        proxy {
          type
          url
          username
          password
        }
        transformOptions {
          forceArrayInstructions
          forceArrayInputs
          replaceDeveloperRoleWithSystem
        }
        passThroughUserAgent
        passThroughBody
        retryableStatusCodes
        retryableErrorPatterns {
          pattern
          regex
        }
        autoModelMappingRules {
          pattern
          publicModelIdTemplate
          createDraft
          developerTemplate
          nameTemplate
          groupTemplate
          modelType
        }
        errorResponseRewriteRules {
          statusCodes
          bodyPattern
          httpStatus
          message
          errorType
          code
          body
        }
      }
      orderingWeight
      errorMessage
      remark
      defaultEndpoints {
        apiFormat
        path
        baseURL
        transport
      }
      endpoints {
        apiFormat
        path
        baseURL
        transport
      }
    }
  }
`;

const PREVIEW_CHANNEL_MODEL_MAPPINGS = `
  query PreviewChannelModelMappings($channelID: ID!) {
    previewChannelModelMappings(channelID: $channelID) {
      channelID expectedVersion createCount skipCount conflictCount
      entries { action from to previousTo reason }
    }
  }
`;

const APPLY_CHANNEL_MODEL_MAPPINGS = `
  mutation ApplyChannelModelMappings($input: ApplyChannelModelMappingsInput!) {
    applyChannelModelMappings(input: $input) {
      channelID expectedVersion createCount skipCount conflictCount
      entries { action from to previousTo reason }
    }
  }
`;

const CHANNEL_MODEL_MAPPING_AUTOMATION_SETTINGS = `
  query ChannelModelMappingAutomationSettings {
    channelModelMappingAutomationSettings { enabled }
  }
`;

const SET_CHANNEL_MODEL_MAPPING_AUTOMATION = `
  mutation SetChannelModelMappingAutomation($input: SetChannelModelMappingAutomationInput!) {
    setChannelModelMappingAutomation(input: $input) { enabled }
  }
`;

const UPDATE_CHANNEL_STATUS_MUTATION = `
  mutation UpdateChannelStatus($id: ID!, $status: ChannelStatus!) {
    updateChannelStatus(id: $id, status: $status) {
      id
      status
    }
  }
`;

const BULK_ARCHIVE_CHANNELS_MUTATION = `
  mutation BulkArchiveChannels($ids: [ID!]!) {
    bulkArchiveChannels(ids: $ids)
  }
`;

const BULK_DISABLE_CHANNELS_MUTATION = `
  mutation BulkDisableChannels($ids: [ID!]!) {
    bulkDisableChannels(ids: $ids)
  }
`;

const BULK_ENABLE_CHANNELS_MUTATION = `
  mutation BulkEnableChannels($ids: [ID!]!) {
    bulkEnableChannels(ids: $ids)
  }
`;

const BULK_RECOVER_CHANNELS_MUTATION = `
  mutation BulkRecoverChannels($ids: [ID!]!) {
    bulkRecoverChannels(ids: $ids)
  }
`;

const DELETE_CHANNEL_MUTATION = `
  mutation DeleteChannel($id: ID!) {
    deleteChannel(id: $id)
  }
`;

const BULK_DELETE_CHANNELS_MUTATION = `
  mutation BulkDeleteChannels($ids: [ID!]!) {
    bulkDeleteChannels(ids: $ids)
  }
`;

const SAVE_CHANNEL_ENDPOINTS_MUTATION = `
  mutation SaveChannelEndpoints($input: SaveChannelEndpointsInput!) {
    saveChannelEndpoints(input: $input) {
      id
      type
      name
      defaultEndpoints {
        apiFormat
        path
        baseURL
        transport
      }
      endpoints {
        apiFormat
        path
        baseURL
        transport
      }
    }
  }
`;

const TEST_CHANNEL_MUTATION = `
  mutation TestChannel($input: TestChannelInput!) {
    testChannel(input: $input) {
      latency
      success
      error
      message
    }
  }
`;

const TEST_CHANNEL_API_KEYS_MUTATION = `
  mutation TestChannelAPIKeys($channelID: ID!, $modelID: String) {
    testChannelAPIKeys(channelID: $channelID, modelID: $modelID) {
      channelID
      total
      successCount
      failedCount
      results {
        keyPrefix
        success
        latency
        error
        disabled
      }
    }
  }
`;

const TEST_CHANNEL_API_KEY_MUTATION = `
  mutation TestChannelAPIKey($channelID: ID!, $key: String!, $modelID: String) {
    testChannelAPIKey(channelID: $channelID, key: $key, modelID: $modelID) {
      keyPrefix
      success
      latency
      error
      disabled
    }
  }
`;

const BULK_IMPORT_CHANNELS_MUTATION = `
  mutation BulkImportChannels($input: BulkImportChannelsInput!) {
    bulkImportChannels(input: $input) {
      success
      created
      failed
      errors
      channels {
        id
        createdAt
        updatedAt
        type
        baseURL
        name
        status
          supportedModels
        autoSyncSupportedModels
        autoSyncModelPattern
        manualModels
        tags
        defaultTestModel
        defaultEndpoints {
          apiFormat
          path
          baseURL
          transport
        }
        endpoints {
          apiFormat
          path
          baseURL
          transport
        }
        settings {
          extraModelPrefix
          modelMappings {
            from
            to
          }
          autoTrimedModelPrefixes
          hideOriginalModels
          hideMappedModels
          lowercaseModelId
          transformOptions {
            forceArrayInstructions
            forceArrayInputs
            replaceDeveloperRoleWithSystem
          }
          passThroughUserAgent
          passThroughBody
          retryableStatusCodes
          retryableErrorPatterns {
            pattern
            regex
          }
          autoModelMappingRules {
            pattern
            publicModelIdTemplate
            createDraft
            developerTemplate
            nameTemplate
            groupTemplate
            modelType
          }
          errorResponseRewriteRules {
            statusCodes
            bodyPattern
            httpStatus
            message
            errorType
            code
            body
          }
        }
      }
    }
  }
`;

// Channel API Key Management Mutations
const DISABLE_CHANNEL_API_KEY_MUTATION = `
  mutation DisableChannelAPIKey($channelID: ID!, $key: String!) {
    disableChannelAPIKey(channelID: $channelID, key: $key)
  }
`;

const ENABLE_CHANNEL_API_KEY_MUTATION = `
  mutation EnableChannelAPIKey($channelID: ID!, $key: String!) {
    enableChannelAPIKey(channelID: $channelID, key: $key)
  }
`;

const ENABLE_ALL_CHANNEL_API_KEYS_MUTATION = `
  mutation EnableAllChannelAPIKeys($channelID: ID!) {
    enableAllChannelAPIKeys(channelID: $channelID)
  }
`;

const ENABLE_SELECTED_CHANNEL_API_KEYS_MUTATION = `
  mutation EnableSelectedChannelAPIKeys($channelID: ID!, $keys: [String!]!) {
    enableSelectedChannelAPIKeys(channelID: $channelID, keys: $keys)
  }
`;

const DELETE_DISABLED_CHANNEL_API_KEYS_MUTATION = `
  mutation DeleteDisabledChannelAPIKeys($channelID: ID!, $keys: [String!]!) {
    deleteDisabledChannelAPIKeys(channelID: $channelID, keys: $keys) {
      success
      message
    }
  }
`;

const GET_CHANNEL_DISABLED_API_KEYS_QUERY = `
  query GetChannelDisabledAPIKeys($id: ID!) {
    node(id: $id) {
      ... on Channel {
        id
        disabledAPIKeys {
          key
          disabledAt
          errorCode
          reason
        }
      }
    }
  }
`;

const GET_CHANNEL_MODEL_PRICES_QUERY = `
  query GetChannelModelPrices($id: ID!) {
    node(id: $id) {
    ... on Channel {
      id
      channelModelPrices {
        id
        modelID
        currencyCode
        price {
          items {
            itemCode
            pricing {
              mode
              flatFee
              usagePerUnit
              usageTiered {
                tiers {
                  upTo
                  pricePerUnit
                }
              }
            }
            promptWriteCacheVariants {
              variantCode
              pricing {
                mode
                flatFee
                usagePerUnit
                usageTiered {
                  tiers {
                    upTo
                    pricePerUnit
                  }
                }
              }
            }
          }
        }
      }
    }
  }
}
`;

const CREATE_PROVIDER_PRICE_CHANGE_SET_MUTATION = `
  mutation CreateProviderPriceChangeSet($channelID: ID!, $input: [SaveChannelModelPriceInput!]!) {
    createProviderPriceChangeSet(channelID: $channelID, input: $input) {
      id
      status
      title
      items { id }
    }
  }
`;

const BULK_UPDATE_CHANNEL_ORDERING_MUTATION = `
  mutation BulkUpdateChannelOrdering($input: BulkUpdateChannelOrderingInput!) {
    bulkUpdateChannelOrdering(input: $input) {
      success
      updated
      channels {
        id
        createdAt
        updatedAt
        type
        baseURL
        name
        status
        supportedModels
        autoSyncSupportedModels
        manualModels
        defaultTestModel
        orderingWeight
        defaultEndpoints {
          apiFormat
          path
          baseURL
          transport
        }
        endpoints {
          apiFormat
          path
          baseURL
          transport
        }
        settings {
          extraModelPrefix
          modelMappings {
            from
            to
          }
          autoTrimedModelPrefixes
          hideOriginalModels
          hideMappedModels
          lowercaseModelId
          transformOptions {
            forceArrayInstructions
            forceArrayInputs
            replaceDeveloperRoleWithSystem
          }
          passThroughUserAgent
          passThroughBody
          retryableStatusCodes
          retryableErrorPatterns {
            pattern
            regex
          }
          autoModelMappingRules {
            pattern
            publicModelIdTemplate
            createDraft
            developerTemplate
            nameTemplate
            groupTemplate
            modelType
          }
          errorResponseRewriteRules {
            statusCodes
            bodyPattern
            httpStatus
            message
            errorType
            code
            body
          }
        }
      }
    }
  }
`;

const ALL_CHANNEL_SUMMARYS_QUERY = `
  query GetAllChannelSummarys($includeArchived: Boolean) {
    allChannelSummarys(includeArchived: $includeArchived) {
      id
      name
      type
      status
      baseURL
      orderingWeight
      tags
      endpoints {
        apiFormat
        path
        baseURL
        transport
      }
      allModelEntries {
        requestModel
        actualModel
        source
      }
    }
  }
`;

const FETCH_MODELS_QUERY = `
  query FetchModels($input: FetchModelsInput!) {
    fetchModels(input: $input) {
      models {
        id
      }
      error
    }
  }
`;

const CHANNEL_TYPES_QUERY = `
  query CountChannelsByType($input: CountChannelsByTypeInput!) {
    countChannelsByType(input: $input) {
      type
      count
    }
  }
`;

const ALL_CHANNEL_TAGS_QUERY = `
  query AllChannelTags {
    allChannelTags
  }
`;

const QUERY_CHANNELS_QUERY = `
  query QueryChannels($input: QueryChannelInput!) {
    queryChannels(input: $input) {
      edges {
        node {
          id
          createdAt
          updatedAt
          type
          baseURL
          websiteURL
          quotaCurrency
          actualQuotaUsed
          quotaRemaining
          name
          status
          policies {
            stream
          }
          credentials {
            apiKey
            apiKeys
            gcp {
              region
              projectID
              jsonData
            }
          }
          supportedModels
          autoSyncSupportedModels
          autoSyncModelPattern
          manualModels
          tags
          defaultTestModel
          settings {
            managementAdapter
            billingCurrency
            rechargeMultiplier
            extraModelPrefix
            modelMappings {
              from
              to
            }
            autoTrimedModelPrefixes
            hideOriginalModels
            hideMappedModels
            lowercaseModelId
            bodyOverrideOperations {
              op
              path
              from
              to
              value
              condition
              match {
                path
                eq
              }
              index
              splat
            }
            headerOverrideOperations {
              op
              path
              from
              to
              value
              condition
              match {
                path
                eq
              }
              index
              splat
            }
            proxy {
              type
              url
              username
              password
            }
            transformOptions {
              forceArrayInstructions
              forceArrayInputs
              replaceDeveloperRoleWithSystem
            }
            passThroughUserAgent
            passThroughBody
            rateLimit {
              rpm
              tpm
              maxConcurrent
              queueSize
              queueTimeoutMs
            }
            retryableStatusCodes
            retryableErrorPatterns {
              pattern
              regex
            }
            autoModelMappingRules {
              pattern
              publicModelIdTemplate
              createDraft
              developerTemplate
              nameTemplate
              groupTemplate
              modelType
            }
            errorResponseRewriteRules {
              statusCodes
              bodyPattern
              httpStatus
              message
              errorType
              code
              body
            }
          }
          orderingWeight
          errorMessage
          remark
          defaultEndpoints {
            apiFormat
            path
            baseURL
            transport
          }
          endpoints {
            apiFormat
            path
            baseURL
            transport
          }
          disabledAPIKeys {
            key
            disabledAt
            errorCode
            reason
          }
          liveLimiterStats {
            inFlight
            waiting
            capacity
            queueSize
          }
          providerQuotaStatus {
            status
            quotaData
            updatedAt
            probeAdapter
            probeVerifiedAt
          }
          operationalIssue {
            category
            severity
            code
            source
          }
        }
        cursor
      }
      pageInfo {
        hasNextPage
        hasPreviousPage
        startCursor
        endCursor
      }
      totalCount
    }
  }
`;

const PROBE_CHANNEL_QUOTA_MUTATION = `
  mutation ProbeChannelQuota($channelID: ID!, $newApiPAT: String, $newApiUserID: ID) {
    probeChannelQuota(channelID: $channelID, newApiPAT: $newApiPAT, newApiUserID: $newApiUserID) {
      success
      adapter
      message
      currency
      total
      used
      remaining
      unlimited
      unlimitedKeyCount
      keyCount
      requiresPat
      balanceSource
      verified
      verifiedAt
    }
  }
`;

const CONFIRM_CHANNEL_QUOTA_PROBE_MUTATION = `
  mutation ConfirmChannelQuotaProbe($channelID: ID!) {
    confirmChannelQuotaProbe(channelID: $channelID) {
      success
      adapter
      message
      currency
      total
      used
      remaining
      unlimited
      unlimitedKeyCount
      keyCount
      requiresPat
      balanceSource
      verified
      verifiedAt
    }
  }
`;

const PROBE_NEW_API_PRICING_MUTATION = `
  mutation ProbeNewApiPricing($channelID: ID!, $newApiPAT: String, $newApiUserID: ID) {
    probeNewApiPricing(channelID: $channelID, newApiPAT: $newApiPAT, newApiUserID: $newApiUserID) {
      source
      sourceEndpoint
      fetchedAt
      pricingVersion
      accountGroup
      effectiveGroups
      keyCount
      matchedKeyCount
      warnings
      models {
        modelId
        billingKind
        quality
        groupRatio
        inputPerMillion
        outputPerMillion
        cacheReadPerMillion
        cacheWritePerMillion
        flatPerRequest
        reason
      }
    }
  }
`;

export function useProbeChannelQuota() {
  const queryClient = useQueryClient();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async ({ channelID, newApiPAT, newApiUserID }: { channelID: string; newApiPAT?: string; newApiUserID?: string }) => {
      const data = await graphqlRequest<{ probeChannelQuota: ChannelQuotaProbeResult }>(PROBE_CHANNEL_QUOTA_MUTATION, {
        channelID,
        newApiPAT,
        newApiUserID,
      });
      return channelQuotaProbeResultSchema.parse(data.probeChannelQuota);
    },
    onSuccess: (result, { channelID }) => {
      if (result.success) {
        queryClient.invalidateQueries({ queryKey: ['channels'] });
        queryClient.invalidateQueries({ queryKey: ['channel', channelID] });
      }
    },
    onError: (error) => handleError(error, { context: 'Probe Channel Quota' }),
  });
}

export function useConfirmChannelQuotaProbe() {
  const queryClient = useQueryClient();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async (channelID: string) => {
      const data = await graphqlRequest<{ confirmChannelQuotaProbe: ChannelQuotaProbeResult }>(CONFIRM_CHANNEL_QUOTA_PROBE_MUTATION, {
        channelID,
      });
      return channelQuotaProbeResultSchema.parse(data.confirmChannelQuotaProbe);
    },
    onSuccess: (result, channelID) => {
      if (result.success) {
        queryClient.invalidateQueries({ queryKey: ['channels'] });
        queryClient.invalidateQueries({ queryKey: ['channel', channelID] });
      }
    },
    onError: (error) => handleError(error, { context: 'Confirm Channel Quota Probe' }),
  });
}

export function useProbeNewApiPricing() {
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async ({ channelID, newApiPAT, newApiUserID }: { channelID: string; newApiPAT?: string; newApiUserID?: string }) => {
      const data = await graphqlRequest<{ probeNewApiPricing: NewApiPricingProbeResult }>(PROBE_NEW_API_PRICING_MUTATION, {
        channelID,
        newApiPAT,
        newApiUserID,
      });
      return newApiPricingProbeResultSchema.parse(data.probeNewApiPricing);
    },
    onError: (error) => handleError(error, { context: 'Probe NEW API Pricing' }),
  });
}

export function useChannelModelPrices(channelId: string) {
  const { handleError } = useErrorHandler();
  const { t } = useTranslation();

  return useQuery({
    queryKey: ['channelModelPrices', channelId],
    queryFn: async () => {
      try {
        const data = await graphqlRequest<{ node: { channelModelPrices: ChannelModelPrice[] } }>(GET_CHANNEL_MODEL_PRICES_QUERY, {
          id: channelId,
        });
        const node = data.node as { channelModelPrices: ChannelModelPrice[] };
        return (node?.channelModelPrices || []).map((p) => channelModelPriceSchema.parse(p));
      } catch (error) {
        handleError(error, t('common.errors.internalServerError'));
        throw error;
      }
    },
    enabled: !!channelId,
  });
}

export function useCreateProviderPriceChangeSet() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async ({ channelId, input }: { channelId: string; input: SaveChannelModelPriceInput[] }) => {
      try {
        const data = await graphqlRequest<{
          createProviderPriceChangeSet: { id: string; status: string; title: string; items: Array<{ id: string }> };
        }>(CREATE_PROVIDER_PRICE_CHANGE_SET_MUTATION, { channelID: channelId, input });
        return data.createProviderPriceChangeSet;
      } catch (error) {
        handleError(error, { context: 'Create Provider Price Change Set' });
        throw error;
      }
    },
    onSuccess: async () => {
      await Promise.all([queryClient.invalidateQueries({ queryKey: ['changeSets'] })]);
      toast.success(t('channels.messages.priceDraftCreated'));
    },
  });
}

// Use this hook to query channels with pagination and filtering
export type ChannelOrderField = 'CREATED_AT' | 'UPDATED_AT' | 'ORDERING_WEIGHT' | 'NAME' | 'STATUS' | 'TYPE';

export function useQueryChannels(
  variables?: {
    first?: number;
    after?: string;
    before?: string;
    last?: number;
    where?: Record<string, unknown>;
    orderBy?: {
      field: ChannelOrderField;
      direction: 'ASC' | 'DESC';
    };
    hasTag?: string;
    model?: string;
  },
  options?: {
    disableAutoFetch?: boolean;
  }
) {
  const { handleError } = useErrorHandler();
  const { t } = useTranslation();

  return useQuery({
    enabled: !options?.disableAutoFetch,
    queryKey: ['channels', variables],
    queryFn: async () => {
      try {
        const data = await graphqlRequest<{ queryChannels: ChannelConnection }>(QUERY_CHANNELS_QUERY, { input: variables });
        return channelConnectionSchema.parse(data?.queryChannels);
      } catch (error) {
        handleError(error, t('common.errors.internalServerError'));
        throw error;
      }
    },
    // Poll so the live limiter snapshot (in-flight / queue) stays roughly fresh.
    // 5s is light traffic; pause when the tab is hidden.
    refetchInterval: 5000,
    refetchIntervalInBackground: false,
  });
}

export function useAllChannelNames(options?: { enabled?: boolean }) {
  const { handleError } = useErrorHandler();
  const { t } = useTranslation();

  return useQuery({
    enabled: options?.enabled ?? true,
    queryKey: ['channels', 'names'],
    queryFn: async () => {
      try {
        const names: string[] = [];
        let after: string | undefined;

        for (;;) {
          const data = await graphqlRequest<{ queryChannels: unknown }>(QUERY_CHANNEL_NAMES_QUERY, {
            input: {
              first: 200,
              after,
              where: {
                statusIn: ['enabled', 'disabled', 'archived'],
              },
            },
          });

          const parsed = channelNamesConnectionSchema.parse(data?.queryChannels);
          names.push(...parsed.edges.map((e) => e.node.name));

          if (!parsed.pageInfo.hasNextPage || !parsed.pageInfo.endCursor) {
            break;
          }

          after = parsed.pageInfo.endCursor;
        }

        return names;
      } catch (error) {
        handleError(error, t('common.errors.internalServerError'));
        throw error;
      }
    },
    staleTime: 5 * 60 * 1000, // 5 minutes
  });
}

// Mutation hooks
export function useCreateChannel() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async (input: CreateChannelInput) => {
      const data = await graphqlRequest<{ createChannel: Channel }>(CREATE_CHANNEL_MUTATION, { input });
      return channelSchema.parse(data.createChannel);
    },
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ['channels'] }),
        queryClient.invalidateQueries({ queryKey: ['commercialization-catalog'] }),
        queryClient.invalidateQueries({ queryKey: ['upstream-supply-catalog'] }),
      ]);
      toast.success(t('channels.messages.createSuccess'));
    },
    onError: (error) => {
      handleError(error, { context: t('channels.dialogs.create.title') });
    },
  });
}

export function useDuplicateChannel() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async ({ sourceID, input }: { sourceID: string; input: CreateChannelInput }) => {
      const data = await graphqlRequest<{ duplicateChannel: Channel }>(DUPLICATE_CHANNEL_MUTATION, { sourceID, input });
      return channelSchema.parse(data.duplicateChannel);
    },
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ['channels'] }),
        queryClient.invalidateQueries({ queryKey: ['changeSets'] }),
      ]);
      toast.success(t('common.success.duplicated'));
    },
    onError: (error) => {
      handleError(error, { context: t('common.actions.duplicate') });
    },
  });
}

export interface BulkCreateChannelsInput {
  type: string;
  name: string;
  baseURL?: string;
  tags?: string[];
  apiKeys: string[];
  supportedModels: string[];
  autoSyncSupportedModels?: boolean;
  defaultTestModel: string;
  settings?: ChannelSettings;
  policies?: ChannelPolicies;
  orderingWeight?: number;
  remark?: string;
}

export function useBulkCreateChannels() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async (input: BulkCreateChannelsInput) => {
      try {
        const data = await graphqlRequest<{ bulkCreateChannels: Channel[] }>(BULK_CREATE_CHANNELS_MUTATION, { input });
        return data.bulkCreateChannels.map((ch) => channelSchema.parse(ch));
      } catch (error) {
        handleError(error, { context: 'Batch Create Channels' });
        throw error;
      }
    },
    onSuccess: (channels) => {
      queryClient.invalidateQueries({ queryKey: ['channels'] });
      toast.success(t('channels.messages.batchCreateSuccess', { count: channels.length }));
    },
  });
}

export function useUpdateChannel() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async ({ id, input }: { id: string; input: UpdateChannelInput }) => {
      const data = await graphqlRequest<{ updateChannel: Channel }>(UPDATE_CHANNEL_MUTATION, { id, input });
      return channelSchema.parse(data.updateChannel);
    },
    onSuccess: async (data) => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ['channels'] }),
        queryClient.invalidateQueries({ queryKey: ['channel', data.id] }),
        queryClient.invalidateQueries({ queryKey: ['commercialization-catalog'] }),
        queryClient.invalidateQueries({ queryKey: ['upstream-supply-catalog'] }),
      ]);
      toast.success(t('channels.messages.updateSuccess'));
    },
    onError: (error) => {
      handleError(error, { context: t('channels.dialogs.edit.title') });
    },
  });
}

export type ChannelModelMappingPreview = {
  channelID: string;
  expectedVersion: string;
  createCount: number;
  skipCount: number;
  conflictCount: number;
  entries: Array<{
    action: 'CREATE' | 'SKIP' | 'CONFLICT';
    from: string;
    to: string;
    previousTo?: string | null;
    reason: string;
  }>;
};

export function usePreviewChannelModelMappings() {
  return useMutation({
    mutationFn: async (channelID: string) => {
      const data = await graphqlRequest<{ previewChannelModelMappings: ChannelModelMappingPreview }>(PREVIEW_CHANNEL_MODEL_MAPPINGS, {
        channelID,
      });
      return data.previewChannelModelMappings;
    },
  });
}

export function useApplyChannelModelMappings() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (input: { channelID: string; expectedVersion: string; replaceConflicts: boolean }) => {
      const data = await graphqlRequest<{ applyChannelModelMappings: ChannelModelMappingPreview }>(APPLY_CHANNEL_MODEL_MAPPINGS, { input });
      return data.applyChannelModelMappings;
    },
    onSuccess: (_data, input) => {
      queryClient.invalidateQueries({ queryKey: ['channels'] });
      queryClient.invalidateQueries({ queryKey: ['channel', input.channelID] });
      queryClient.invalidateQueries({ queryKey: ['commercialization-catalog'] });
    },
  });
}

export function useChannelModelMappingAutomationSettings(enabled = true) {
  return useQuery({
    queryKey: ['channel-model-mapping-automation-settings'],
    queryFn: async () => {
      const data = await graphqlRequest<{ channelModelMappingAutomationSettings: { enabled: boolean } }>(
        CHANNEL_MODEL_MAPPING_AUTOMATION_SETTINGS
      );
      return data.channelModelMappingAutomationSettings;
    },
    enabled,
  });
}

export function useSetChannelModelMappingAutomation() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (enabled: boolean) => {
      const data = await graphqlRequest<{ setChannelModelMappingAutomation: { enabled: boolean } }>(SET_CHANNEL_MODEL_MAPPING_AUTOMATION, {
        input: { enabled },
      });
      return data.setChannelModelMappingAutomation;
    },
    onSuccess: (settings) => {
      queryClient.setQueryData(['channel-model-mapping-automation-settings'], settings);
    },
  });
}

export interface SaveChannelEndpointsInput {
  channelID: string;
  endpoints: Array<{ apiFormat: string; path?: string; baseURL?: string; transport?: string }>;
}

export function useSaveChannelEndpoints() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async (input: SaveChannelEndpointsInput) => {
      const data = await graphqlRequest<{ saveChannelEndpoints: Channel }>(SAVE_CHANNEL_ENDPOINTS_MUTATION, { input });
      return channelEndpointsResponseSchema.parse(data.saveChannelEndpoints);
    },
    onSuccess: (_data, variables) => {
      queryClient.invalidateQueries({ queryKey: ['channels'] });
      queryClient.invalidateQueries({ queryKey: ['channel', variables.channelID] });
      toast.success(t('channels.messages.updateSuccess'));
    },
    onError: (error) => {
      handleError(error, { context: t('channels.dialogs.edit.title') });
    },
  });
}

export function useClearChannelErrorMessage() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async ({ id }: { id: string }) => {
      try {
        const data = await graphqlRequest<{ updateChannel: Channel }>(UPDATE_CHANNEL_MUTATION, {
          id,
          input: { clearErrorMessage: true },
        });
        return channelSchema.parse(data.updateChannel);
      } catch (error) {
        handleError(error, { context: 'Clear Channel Error' });
        throw error;
      }
    },
    onSuccess: (data) => {
      queryClient.invalidateQueries({ queryKey: ['channels'] });
      queryClient.invalidateQueries({ queryKey: ['channel', data.id] });
      queryClient.invalidateQueries({ queryKey: ['errorChannelsCount'] });
      toast.success(t('channels.messages.errorResolvedSuccess'));
    },
  });
}

export function useUpdateChannelStatus() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async ({ id, status }: { id: string; status: 'enabled' | 'disabled' | 'archived' }) => {
      try {
        const data = await graphqlRequest<{ updateChannelStatus: boolean }>(UPDATE_CHANNEL_STATUS_MUTATION, {
          id,
          status,
        });
        return data.updateChannelStatus;
      } catch (error) {
        handleError(error, { context: 'Update Channel Status' });
        throw error;
      }
    },
    onSuccess: (_data, variables) => {
      queryClient.invalidateQueries({ queryKey: ['channels'] });
      const statusText =
        variables.status === 'enabled'
          ? t('channels.status.enabled')
          : variables.status === 'archived'
            ? t('channels.status.archived')
            : t('channels.status.disabled');

      const messageKey = variables.status === 'archived' ? 'channels.messages.archiveSuccess' : 'channels.messages.statusUpdateSuccess';

      toast.success(variables.status === 'archived' ? t(messageKey) : t(messageKey, { status: statusText }));
    },
  });
}

export function useBulkArchiveChannels() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async (ids: string[]) => {
      try {
        const data = await graphqlRequest<{ bulkArchiveChannels: boolean }>(BULK_ARCHIVE_CHANNELS_MUTATION, { ids });
        return data.bulkArchiveChannels;
      } catch (error) {
        handleError(error, { context: 'Bulk Archive Channels' });
        throw error;
      }
    },
    onSuccess: (_data, variables) => {
      queryClient.invalidateQueries({ queryKey: ['channels'] });
      toast.success(t('channels.messages.bulkArchiveSuccess', { count: variables.length }));
    },
  });
}

export function useBulkDisableChannels() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async (ids: string[]) => {
      try {
        const data = await graphqlRequest<{ bulkDisableChannels: boolean }>(BULK_DISABLE_CHANNELS_MUTATION, { ids });
        return data.bulkDisableChannels;
      } catch (error) {
        handleError(error, { context: 'Bulk Disable Channels' });
        throw error;
      }
    },
    onSuccess: (_data, variables) => {
      queryClient.invalidateQueries({ queryKey: ['channels'] });
      toast.success(t('channels.messages.bulkDisableSuccess', { count: variables.length }));
    },
  });
}

export function useBulkEnableChannels() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async (ids: string[]) => {
      try {
        const data = await graphqlRequest<{ bulkEnableChannels: boolean }>(BULK_ENABLE_CHANNELS_MUTATION, { ids });
        return data.bulkEnableChannels;
      } catch (error) {
        handleError(error, { context: 'Bulk Enable Channels' });
        throw error;
      }
    },
    onSuccess: (_data, variables) => {
      queryClient.invalidateQueries({ queryKey: ['channels'] });
      toast.success(t('channels.messages.bulkEnableSuccess', { count: variables.length }));
    },
  });
}

export function useBulkRecoverChannels() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async (ids: string[]) => {
      try {
        const data = await graphqlRequest<{ bulkRecoverChannels: boolean }>(BULK_RECOVER_CHANNELS_MUTATION, { ids });
        return data.bulkRecoverChannels;
      } catch (error) {
        handleError(error, { context: 'Bulk Recover Channels' });
        throw error;
      }
    },
    onSuccess: (_data, variables) => {
      queryClient.invalidateQueries({ queryKey: ['channels'] });
      queryClient.invalidateQueries({ queryKey: ['errorChannelsCount'] });
      toast.success(t('channels.messages.bulkRecoverSuccess', { count: variables.length }));
    },
  });
}

export function useDeleteChannel() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async (id: string) => {
      try {
        const data = await graphqlRequest<{ deleteChannel: boolean }>(DELETE_CHANNEL_MUTATION, { id });
        return data.deleteChannel;
      } catch (error) {
        handleError(error, { context: 'Delete Channel' });
        throw error;
      }
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['channels'] });
      toast.success(t('channels.messages.deleteSuccess'));
    },
  });
}

export function useBulkDeleteChannels() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async (ids: string[]) => {
      try {
        const data = await graphqlRequest<{ bulkDeleteChannels: boolean }>(BULK_DELETE_CHANNELS_MUTATION, { ids });
        return data.bulkDeleteChannels;
      } catch (error) {
        handleError(error, { context: 'Bulk Delete Channels' });
        throw error;
      }
    },
    onSuccess: (_data, variables) => {
      queryClient.invalidateQueries({ queryKey: ['channels'] });
      toast.success(t('channels.messages.bulkDeleteSuccess', { count: variables.length }));
    },
  });
}

export function useTestChannel(options?: { silent?: boolean }) {
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();
  const silent = options?.silent ?? false;

  return useMutation({
    mutationFn: async ({
      channelID,
      modelID,
      proxy,
    }: {
      channelID: string;
      modelID?: string;
      proxy?: { type: string; url?: string; username?: string; password?: string };
    }) => {
      try {
        const data = await graphqlRequest<{
          testChannel: {
            latency: number;
            success: boolean;
            message?: string | null;
            error?: string | null;
          };
        }>(TEST_CHANNEL_MUTATION, { input: { channelID, modelID, proxy } });
        return {
          ...data.testChannel,
          error: localizeChannelTestError(data.testChannel.error, t('channels.messages.invalidOfficialOpenAIKey')),
        };
      } catch (error) {
        if (!silent) {
          handleError(error, { context: 'Test Channel' });
        }
        throw error;
      }
    },
    onSuccess: (data) => {
      if (silent) {
        return;
      }

      if (data.success) {
        toast.success(t('channels.messages.testSuccess', { latency: data.latency.toFixed(2) }));
      } else {
        // Handle case where GraphQL request succeeds but test fails
        const errorMsg = data.error || t('common.errors.internalServerError');
        toast.error(errorMsg);
      }
    },
  });
}

export function useTestChannelAPIKeys(options?: { silent?: boolean }) {
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();
  const silent = options?.silent ?? false;

  return useMutation({
    mutationFn: async ({ channelID, modelID }: { channelID: string; modelID?: string }) => {
      try {
        const data = await graphqlRequest<{ testChannelAPIKeys: TestChannelAPIKeysPayload }>(TEST_CHANNEL_API_KEYS_MUTATION, {
          channelID,
          modelID,
        });
        const payload = testChannelAPIKeysPayloadSchema.parse(data.testChannelAPIKeys);
        return {
          ...payload,
          results: payload.results.map((result) => ({
            ...result,
            error: localizeChannelTestError(result.error, t('channels.messages.invalidOfficialOpenAIKey')),
          })),
        };
      } catch (error) {
        if (!silent) {
          handleError(error, { context: 'Test Channel API Keys' });
        }
        throw error;
      }
    },
    onSuccess: (data) => {
      if (silent) {
        return;
      }

      if (data.failedCount === 0) {
        toast.success(t('channels.dialogs.testAPIKeys.successSummary', { success: data.successCount, total: data.total }));
        return;
      }

      toast.error(t('channels.dialogs.testAPIKeys.successSummary', { success: data.successCount, total: data.total }));
    },
  });
}

export function useTestChannelAPIKey() {
  const { t } = useTranslation();
  return useMutation({
    mutationFn: async ({ channelID, key, modelID }: { channelID: string; key: string; modelID?: string }) => {
      const data = await graphqlRequest<{ testChannelAPIKey: TestAPIKeyResult }>(TEST_CHANNEL_API_KEY_MUTATION, {
        channelID,
        key,
        modelID,
      });
      const result = testAPIKeyResultSchema.parse(data.testChannelAPIKey);
      return {
        ...result,
        error: localizeChannelTestError(result.error, t('channels.messages.invalidOfficialOpenAIKey')),
      };
    },
  });
}

export function useBulkImportChannels() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async (input: BulkImportChannelsInput) => {
      try {
        const data = await graphqlRequest<{ bulkImportChannels: BulkImportChannelsResult }>(BULK_IMPORT_CHANNELS_MUTATION, { input });
        return bulkImportChannelsResultSchema.parse(data.bulkImportChannels);
      } catch (error) {
        handleError(error, { context: 'Bulk Import Channels' });
        throw error;
      }
    },
    onSuccess: (data) => {
      queryClient.invalidateQueries({ queryKey: ['channels'] });

      if (data.success) {
        toast.success(
          t('channels.messages.bulkImportSuccess', {
            created: data.created,
          })
        );
      } else {
        toast.error(
          t('channels.messages.bulkImportPartialError', {
            created: data.created,
            failed: data.failed,
          })
        );
      }
    },
  });
}

export function useAllChannelSummarys(projectId?: string | null, options?: { enabled?: boolean; includeArchived?: boolean }) {
  const { handleError } = useErrorHandler();
  const { t } = useTranslation();

  return useQuery({
    queryKey: ['allChannelSummarys', projectId, options?.includeArchived],
    queryFn: async () => {
      try {
        const headers = projectId ? { 'X-Project-ID': projectId } : undefined;
        const data = await graphqlRequest<{ allChannelSummarys: ChannelSummaryConnection['edges'][number]['node'][] }>(
          ALL_CHANNEL_SUMMARYS_QUERY,
          { includeArchived: options?.includeArchived },
          headers
        );
        return channelSummaryConnectionSchema.parse({
          edges: (data?.allChannelSummarys || []).map((node) => ({ node })),
          totalCount: data?.allChannelSummarys?.length || 0,
        });
      } catch (error) {
        handleError(error, t('common.errors.internalServerError'));
        throw error;
      }
    },
    enabled: options?.enabled !== false,
  });
}

export function useBulkUpdateChannelOrdering() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async (input: BulkUpdateChannelOrderingInput) => {
      try {
        const data = await graphqlRequest<{ bulkUpdateChannelOrdering: BulkUpdateChannelOrderingResult }>(
          BULK_UPDATE_CHANNEL_ORDERING_MUTATION,
          { input }
        );
        return bulkUpdateChannelOrderingResultSchema.parse(data.bulkUpdateChannelOrdering);
      } catch (error) {
        handleError(error, { context: 'Update Channel Ordering' });
        throw error;
      }
    },
    onSuccess: (data) => {
      queryClient.invalidateQueries({ queryKey: ['channels'] });
      queryClient.invalidateQueries({ queryKey: ['allChannelSummarys'] });
      toast.success(
        t('channels.messages.orderingUpdateSuccess', {
          updated: data.updated,
        })
      );
    },
  });
}

const SYNC_CHANNEL_MODELS_MUTATION = `
  mutation SyncChannelModels($channelID: ID!, $pattern: String) {
    syncChannelModels(channelID: $channelID, pattern: $pattern) {
      channelID
      supportedModels
    }
  }
`;

const syncChannelModelsPayloadSchema = z.object({
  channelID: z.string(),
  supportedModels: z.array(z.string()),
});

export function useSyncChannelModels() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async (input: { channelID: string; pattern?: string }) => {
      try {
        const data = await graphqlRequest<{ syncChannelModels: unknown }>(SYNC_CHANNEL_MODELS_MUTATION, input);
        return syncChannelModelsPayloadSchema.parse(data.syncChannelModels);
      } catch (error) {
        handleError(error, { context: 'Sync Channel Models' });
        throw error;
      }
    },
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ['channels'] }),
        queryClient.invalidateQueries({ queryKey: ['commercialization-catalog'] }),
        queryClient.invalidateQueries({ queryKey: ['upstream-supply-catalog'] }),
      ]);
      toast.success(t('channels.messages.syncModelsSuccess'));
    },
  });
}

export function useFetchModels() {
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async (input: { channelType: string; baseURL: string; apiKey?: string; channelID?: string }) => {
      try {
        const data = await graphqlRequest<{
          fetchModels: {
            models: Array<{ id: string }>;
            error?: string | null;
          };
        }>(FETCH_MODELS_QUERY, { input });
        return data.fetchModels;
      } catch (error) {
        handleError(error, { context: 'Fetch Models' });
        throw error;
      }
    },
    onSuccess: (data) => {
      if (data.error) {
        toast.error(t('common.errors.internalServerError'));
      } else {
        const count = data.models.length;
        if (count > 100) {
          toast.success(t('channels.messages.fetchModelsSuccessLarge', { count }));
        } else {
          toast.success(t('channels.messages.fetchModelsSuccess', { count }));
        }
      }
    },
  });
}

export interface ChannelTypeCount {
  type: string;
  count: number;
}

export function useChannelTypes(statusIn?: string[]) {
  const { handleError } = useErrorHandler();
  const { t } = useTranslation();

  return useQuery({
    queryKey: ['channelTypes', statusIn],
    queryFn: async () => {
      try {
        const input: { statusIn?: string[] } = {};
        if (statusIn && statusIn.length > 0) {
          input.statusIn = statusIn;
        }
        const data = await graphqlRequest<{ countChannelsByType: ChannelTypeCount[] }>(CHANNEL_TYPES_QUERY, { input });
        return data.countChannelsByType || [];
      } catch (error) {
        handleError(error, t('common.errors.internalServerError'));
        throw error;
      }
    },
    staleTime: 5 * 60 * 1000, // 5 minutes
  });
}

const ERROR_CHANNELS_COUNT_QUERY = `
  query GetErrorChannelsCount {
    channels(
      first: 1,
      where: { errorMessageNotNil: true }
    ) {
      totalCount
    }
  }
`;

export function useErrorChannelsCount() {
  const { handleError } = useErrorHandler();
  const { t } = useTranslation();

  return useQuery({
    queryKey: ['errorChannelsCount'],
    queryFn: async () => {
      try {
        const data = await graphqlRequest<{ channels: { totalCount: number } }>(ERROR_CHANNELS_COUNT_QUERY);
        return data.channels.totalCount;
      } catch (error) {
        handleError(error, t('common.errors.internalServerError'));
        throw error;
      }
    },
    staleTime: 5 * 60 * 1000, // 5 minutes cache
  });
}

export function useAllChannelTags(projectId?: string | null) {
  const { handleError } = useErrorHandler();
  const { t } = useTranslation();

  return useQuery({
    queryKey: ['allChannelTags', projectId],
    queryFn: async () => {
      try {
        const headers = projectId ? { 'X-Project-ID': projectId } : undefined;
        const data = await graphqlRequest<{ allChannelTags: string[] }>(ALL_CHANNEL_TAGS_QUERY, undefined, headers);
        return data.allChannelTags || [];
      } catch (error) {
        handleError(error, t('common.errors.internalServerError'));
        throw error;
      }
    },
    staleTime: 5 * 60 * 1000, // 5 minutes
  });
}

const CHANNEL_PROBE_DATA_QUERY = `
  query GetChannelProbeData($input: GetChannelProbeDataInput!) {
    channelProbeData(input: $input) {
      channelID
      points {
        timestamp
        totalRequestCount
        successRequestCount
        avgTokensPerSecond
        avgTimeToFirstTokenMs
      }
    }
  }
`;

export function useChannelProbeData(channelIDs: string[], options?: { enabled?: boolean }) {
  const { handleError } = useErrorHandler();
  const { t } = useTranslation();

  return useQuery({
    queryKey: ['channelProbeData', channelIDs],
    queryFn: async () => {
      try {
        const data = await graphqlRequest<{ channelProbeData: any[] }>(CHANNEL_PROBE_DATA_QUERY, {
          input: { channelIDs },
        });
        return data.channelProbeData || [];
      } catch (error) {
        handleError(error, t('common.errors.internalServerError'));
        return [];
      }
    },
    enabled: channelIDs.length > 0 && options?.enabled !== false,
    staleTime: 1 * 60 * 1000, // 1 minute
  });
}

// Channel Disabled API Keys Hooks
export function useChannelDisabledAPIKeys(channelId: string, options?: { enabled?: boolean }) {
  const { handleError } = useErrorHandler();
  const { t } = useTranslation();

  return useQuery({
    queryKey: ['channelDisabledAPIKeys', channelId],
    queryFn: async () => {
      try {
        const data = await graphqlRequest<{
          node: {
            id: string;
            disabledAPIKeys: Array<{
              key: string;
              disabledAt: string;
              errorCode: number;
              reason?: string | null;
            }>;
          };
        }>(GET_CHANNEL_DISABLED_API_KEYS_QUERY, { id: channelId });
        return data.node?.disabledAPIKeys || [];
      } catch (error) {
        handleError(error, t('common.errors.internalServerError'));
        return [];
      }
    },
    enabled: !!channelId && options?.enabled !== false,
  });
}

export function useDisableChannelAPIKey() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async ({ channelID, key }: { channelID: string; key: string }) => {
      try {
        const data = await graphqlRequest<{ disableChannelAPIKey: boolean }>(DISABLE_CHANNEL_API_KEY_MUTATION, {
          channelID,
          key,
        });
        return data.disableChannelAPIKey;
      } catch (error) {
        handleError(error, { context: 'Disable Channel API Key' });
        throw error;
      }
    },
    onSuccess: (_data, variables) => {
      queryClient.invalidateQueries({ queryKey: ['channelDisabledAPIKeys', variables.channelID] });
      queryClient.invalidateQueries({ queryKey: ['channels'] });
      toast.success(t('channels.messages.disableAPIKeySuccess'));
    },
  });
}

export function useEnableChannelAPIKey() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async ({ channelID, key }: { channelID: string; key: string }) => {
      try {
        const data = await graphqlRequest<{ enableChannelAPIKey: boolean }>(ENABLE_CHANNEL_API_KEY_MUTATION, {
          channelID,
          key,
        });
        return data.enableChannelAPIKey;
      } catch (error) {
        handleError(error, { context: 'Enable Channel API Key' });
        throw error;
      }
    },
    onSuccess: (_data, variables) => {
      queryClient.invalidateQueries({ queryKey: ['channelDisabledAPIKeys', variables.channelID] });
      queryClient.invalidateQueries({ queryKey: ['channels'] });
      toast.success(t('channels.messages.enableAPIKeySuccess'));
    },
  });
}

export function useEnableAllChannelAPIKeys() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async ({ channelID }: { channelID: string }) => {
      try {
        const data = await graphqlRequest<{ enableAllChannelAPIKeys: boolean }>(ENABLE_ALL_CHANNEL_API_KEYS_MUTATION, {
          channelID,
        });
        return data.enableAllChannelAPIKeys;
      } catch (error) {
        handleError(error, { context: 'Enable All Channel API Keys' });
        throw error;
      }
    },
    onSuccess: (_data, variables) => {
      queryClient.invalidateQueries({ queryKey: ['channelDisabledAPIKeys', variables.channelID] });
      queryClient.invalidateQueries({ queryKey: ['channels'] });
      toast.success(t('channels.messages.enableAllAPIKeysSuccess'));
    },
  });
}

export function useEnableSelectedChannelAPIKeys() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async ({ channelID, keys }: { channelID: string; keys: string[] }) => {
      try {
        const data = await graphqlRequest<{ enableSelectedChannelAPIKeys: boolean }>(ENABLE_SELECTED_CHANNEL_API_KEYS_MUTATION, {
          channelID,
          keys,
        });
        return data.enableSelectedChannelAPIKeys;
      } catch (error) {
        handleError(error, { context: 'Enable Selected API Keys' });
        throw error;
      }
    },
    onSuccess: (_data, variables) => {
      queryClient.invalidateQueries({ queryKey: ['channelDisabledAPIKeys', variables.channelID] });
      queryClient.invalidateQueries({ queryKey: ['channels'] });
      toast.success(t('channels.messages.enableSelectedAPIKeysSuccess'));
    },
  });
}

export function useDeleteDisabledChannelAPIKeys() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();

  return useMutation({
    mutationFn: async ({ channelID, keys }: { channelID: string; keys: string[] }) => {
      try {
        const data = await graphqlRequest<{ deleteDisabledChannelAPIKeys: { success: boolean; message?: string } }>(
          DELETE_DISABLED_CHANNEL_API_KEYS_MUTATION,
          { channelID, keys }
        );
        return data.deleteDisabledChannelAPIKeys;
      } catch (error) {
        handleError(error, { context: 'Delete Disabled API Keys' });
        throw error;
      }
    },
    onSuccess: (data, variables) => {
      queryClient.invalidateQueries({ queryKey: ['channelDisabledAPIKeys', variables.channelID] });
      queryClient.invalidateQueries({ queryKey: ['channels'] });

      // Show appropriate message based on the result
      if (data.message === 'ONE_KEY_PRESERVED') {
        toast.success(t('channels.messages.deleteDisabledAPIKeysPreserved'));
      } else {
        toast.success(t('channels.messages.deleteDisabledAPIKeysSuccess'));
      }
    },
  });
}
