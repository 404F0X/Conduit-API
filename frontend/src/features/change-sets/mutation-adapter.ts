import type { GovernanceCommand } from './data/governance';

export const GOVERNANCE_REQUEST_EVENT = 'conduit-governance-request';
type Variables = Record<string, unknown>;
function object(value: unknown): Variables {
  return value && typeof value === 'object' && !Array.isArray(value) ? (value as Variables) : {};
}
function snake(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(snake);
  if (!value || typeof value !== 'object') return value;
  return Object.fromEntries(
    Object.entries(value).map(([key, value]) => [
      key
        .replace(/ID(s?)$/, '_id$1')
        .replace(/([a-z0-9])([A-Z])/g, '$1_$2')
        .toLowerCase(),
      snake(value),
    ])
  );
}
function domainInput(value: unknown): unknown {
  const v = snake(value);
  function enums(value: unknown): unknown {
    if (Array.isArray(value)) return value.map(enums);
    if (!value || typeof value !== 'object') return value;
    return Object.fromEntries(
      Object.entries(value).map(([key, value]) => [
        key,
        ['interval_unit', 'quota_class', 'rollover_mode', 'status'].includes(key) && typeof value === 'string'
          ? value.toLowerCase().replace(/(^|_)([a-z])/g, (_, _prefix, letter: string) => letter.toUpperCase())
          : enums(value),
      ])
    );
  }
  return enums(v);
}
function databaseID(value: unknown) {
  const id = Number(String(value).split('/').at(-1));
  if (!Number.isSafeInteger(id) || id <= 0) throw new Error('Invalid resource ID');
  return id;
}
function ids(v: Variables) {
  return Array.isArray(v.ids) ? v.ids.map(String) : v.id ? [String(v.id)] : [];
}
const forms: Record<string, string> = {
  updateProductExperienceSettings: 'system.product_experience_form',
  saveChannelEndpoints: 'channel.endpoints_form',
  bulkUpdateChannelOrdering: 'channel.ordering_form',
  setChannelModelMappingAutomation: 'model.mapping_automation',
  applyChannelModelMappings: 'channel.aliases_form',
  upsertModelRoute: 'model.route_form',
  createPublicModelWithRoutes: 'model.public_form',
  createPriceBook: 'price_book.create_form',
  updateBrandSettings: 'system.brand_form',
  updateSystemGeneralSettings: 'system.general_form',
  updateSystemModelSettings: 'system.model_form',
  updateSystemChannelSettings: 'system.channel_form',
  updatePublicChannelHealthSettings: 'system.public_health_form',
  updateSecuritySettings: 'system.security_form',
  updateStoragePolicy: 'system.storage_form',
  updateRetryPolicy: 'system.retry_form',
  updateWebhookNotifierConfig: 'system.webhook_form',
  updateVideoStorageSettings: 'system.video_form',
  updateAutoBackupSettings: 'system.backup_form',
  updateQuotaEnforcementSettings: 'system.quota_form',
  saveProxyPreset: 'system.proxy_save',
  updateProjectProfiles: 'project.profiles_form',
  createChannelOverrideTemplate: 'channel_template.create_form',
  updateChannelOverrideTemplate: 'channel_template.update_form',
  createAPIKey: 'api_key.create_form',
  updateAPIKey: 'api_key.update_form',
  updateAPIKeyProfiles: 'api_key.profiles_form',
  createDataStorage: 'storage.create_form',
  updateDataStorage: 'storage.update_form',
  createPromptProtectionRule: 'protection.create_form',
  updatePromptProtectionRule: 'protection.update_form',
  createModel: 'model.create_form',
  updateModel: 'model.update_form',
  addUserToProject: 'member.add_form',
  updateProjectUser: 'member.update_form',
  removeUserFromProject: 'member.remove_form',
  createUser: 'user.create_form',
  updateUser: 'user.update_form',
  createProject: 'project.create_form',
  updateProject: 'project.update_form',
  createRole: 'role.create_form',
  updateRole: 'role.update_form',
  createChannel: 'channel.create_form',
  updateChannel: 'channel.update_form',
  bulkCreateChannels: 'channel.bulk_create_form',
  bulkImportChannels: 'channel.import_form',
};
const writes: Record<string, string> = {
  createModel: 'model.write',
  updateModel: 'model.write',
  createProject: 'project.write',
  updateProject: 'project.write',
  createUser: 'user.write',
  updateUser: 'user.write',
  createRole: 'role.write',
  updateRole: 'role.write',
  createSimpleGroup: 'group.create',
  updateSimpleGroup: 'group.update',
  assignSimpleGroupUsers: 'group.assign',
  updateSimpleGroupModels: 'group.models',
  updateSimpleGroupPrice: 'group.price',
  grantProjectCredit: 'project.credit',
  createSubscriptionPlan: 'subscription_plan.create',
  updateSubscriptionPlan: 'subscription_plan.update',
  assignUserSubscription: 'subscription.assign',
  createCreditRedemptionCodes: 'redemption.create',
  createAPIKey: 'api_key.write',
  updateAPIKey: 'api_key.write',
  createAPIKeyProfileTemplate: 'key_template.write',
  updateAPIKeyProfileTemplate: 'key_template.write',
  createChannelOverrideTemplate: 'channel_template.write',
  updateChannelOverrideTemplate: 'channel_template.write',
  createPromptProtectionRule: 'protection.write',
  updatePromptProtectionRule: 'protection.write',
  createDataStorage: 'storage.write',
  updateDataStorage: 'storage.write',
};
const deletes: Record<string, string> = {
  deleteChannel: 'channel.delete',
  bulkDeleteChannels: 'channel.delete',
  deleteModel: 'model.delete',
  bulkDeleteModels: 'model.delete',
  deleteProject: 'project.delete',
  bulkDeleteProjects: 'project.delete',
  deleteUser: 'user.delete',
  bulkDeleteUsers: 'user.delete',
  deleteRole: 'role.delete',
  bulkDeleteRoles: 'role.delete',
  deleteSimpleGroup: 'group.delete',
  rotateAPIKey: 'api_key.rotate',
  deleteAPIKey: 'api_key.delete',
  deleteAPIKeyProfileTemplate: 'key_template.delete',
  deleteChannelOverrideTemplate: 'channel_template.delete',
  clearChannelOverrideTemplate: 'channel_template.clear',
  deletePromptProtectionRule: 'protection.delete',
  cancelUserSubscription: 'subscription.cancel',
  refreshSubscriptionAllowance: 'subscription.refresh',
  pauseUserSubscription: 'subscription.pause',
  resumeUserSubscription: 'subscription.resume',
  renewUserSubscription: 'subscription.renew',
  revokeCreditRedemptionCode: 'redemption.revoke',
};
const settings: Record<string, string> = {
  updateSystemGeneralSettings: 'system_general_settings',
  updateSystemModelSettings: 'system_model_settings',
  updateSystemChannelSettings: 'system_channel_settings',
  updateSecuritySettings: 'security_settings',
  updateStoragePolicy: 'storage_policy',
  updateRetryPolicy: 'retry_policy',
  updateWebhookNotifierConfig: 'webhook_notifier_config',
  updateVideoStorageSettings: 'system_video_storage_settings',
  updateAutoBackupSettings: 'system_auto_backup_settings',
  updateQuotaEnforcementSettings: 'quota_enforcement_settings',
  updateTitle: 'system_title',
  updateBrandName: 'system_brand_name',
  updateBrandLogo: 'system_brand_logo',
};
export function governedMutation(query: string, variables: Variables = {}): GovernanceCommand | null {
  if (!/^\s*mutation\b/.test(query)) return null;
  const field = query.match(/\{\s*(?:\w+\s*:\s*)?(\w+)\s*[({]/)?.[1];
  if (!field) return null;
  const targets = ids(variables);
  if (field === 'createApiKeyProfileTemplate' || field === 'updateApiKeyProfileTemplate')
    return {
      action: field === 'createApiKeyProfileTemplate' ? 'key_template.create_form' : 'key_template.update_form',
      targetIDs: targets,
      payload: { input: object(variables.input), profile: variables.profile ?? null },
    };
  if (field === 'deleteApiKeyProfileTemplate') return { action: 'key_template.delete', targetIDs: targets, payload: {} };
  if (field === 'loadApiKeyProfileTemplate')
    return {
      action: 'key_template.load',
      targetIDs: [String(object(variables.input).apiKeyID)],
      payload: { template_id: databaseID(object(variables.input).templateID) },
    };
  if (field === 'applyChannelOverrideTemplate') {
    const input = object(variables.input);
    return {
      action: 'channel_template.apply',
      targetIDs: Array.isArray(input.channelIDs) ? input.channelIDs.map(String) : [],
      payload: { template_id: databaseID(input.templateID), replace: input.mode !== 'MERGE' },
    };
  }
  if (field === 'clearChannelOverrideTemplates')
    return {
      action: 'channel_template.clear',
      targetIDs: Array.isArray(object(variables.input).channelIDs) ? (object(variables.input).channelIDs as unknown[]).map(String) : [],
      payload: {},
    };
  if (field === 'deleteProxyPreset') return { action: 'system.proxy_delete', targetIDs: [], payload: { url: variables.url } };
  if (field === 'updateDefaultDataStorage')
    return {
      action: 'system.setting',
      targetIDs: [],
      payload: { key: 'default_data_storage_id', value: databaseID(object(variables.input).dataStorageID) },
    };
  if (field === 'updateUserAgentPassThroughSettings' || field === 'updatePassThroughSettings')
    return {
      action: 'system.setting',
      targetIDs: [],
      payload: {
        key: field === 'updateUserAgentPassThroughSettings' ? 'system_user_agent_pass_through' : 'system_pass_through',
        value: object(variables.input).enabled,
      },
    };
  if (/^(disable|enable|enableAll|enableSelected|deleteDisabled)ChannelAPIKeys?$/.test(field))
    return {
      action: 'channel.keys',
      targetIDs: [String(variables.channelID)],
      payload: {
        operation: field.startsWith('disable')
          ? 'disable'
          : field.startsWith('enableAll')
            ? 'enable_all'
            : field.startsWith('delete')
              ? 'delete_disabled'
              : 'enable',
        keys: Array.isArray(variables.keys) ? variables.keys : variables.key ? [variables.key] : [],
      },
    };
  if (
    field === 'updatePromptProtectionRuleStatus' ||
    field === 'bulkEnablePromptProtectionRules' ||
    field === 'bulkDisablePromptProtectionRules'
  )
    return {
      action: 'protection.write',
      targetIDs: targets,
      payload: {
        status: field.startsWith('bulkEnable')
          ? 'enabled'
          : field.startsWith('bulkDisable')
            ? 'disabled'
            : String(variables.status).toLowerCase(),
      },
    };
  if (field === 'bulkDeletePromptProtectionRules') return { action: 'protection.delete', targetIDs: targets, payload: {} };
  if (field === 'setSubscriptionAutoRenew')
    return {
      action: 'subscription.auto_renew',
      targetIDs: [String(variables.subscriptionID ?? variables.id)],
      payload: { auto_renew: variables.autoRenew },
    };
  if (forms[field])
    return {
      action: forms[field],
      targetIDs: targets,
      payload: object(variables.input ?? variables.settings ?? variables.policy ?? variables.config ?? variables),
    };
  if (writes[field]) return { action: writes[field], targetIDs: targets, payload: object(domainInput(variables.input ?? variables)) };
  if (deletes[field])
    return {
      action: deletes[field],
      targetIDs: targets.length
        ? targets
        : [String(variables.subscriptionID ?? variables.codeID ?? variables.groupID ?? '')].filter(Boolean),
      payload: {},
    };
  const status =
    /^(?:bulk)?(Enable|Disable)/.exec(field)?.[1]?.toLowerCase() ??
    (field.startsWith('bulkEnable')
      ? 'enabled'
      : field.startsWith('bulkDisable')
        ? 'disabled'
        : field.startsWith('bulkArchive')
          ? 'archived'
          : field.startsWith('bulkRecover')
            ? 'enabled'
            : variables.status);
  const entity = /(?:Channels?|Models?|Projects?|Users?|APIKeys?)/.exec(field)?.[0];
  if (
    field.includes('Status') ||
    field.startsWith('bulkEnable') ||
    field.startsWith('bulkDisable') ||
    field.startsWith('bulkArchive') ||
    field.startsWith('bulkRecover')
  ) {
    const action = entity?.startsWith('Channel')
      ? 'channel.write'
      : entity?.startsWith('Model')
        ? 'model.write'
        : entity?.startsWith('Project')
          ? 'project.write'
          : entity?.startsWith('User')
            ? 'user.write'
            : entity?.startsWith('APIKey')
              ? 'api_key.write'
              : undefined;
    if (action)
      return {
        action,
        targetIDs: targets,
        payload: {
          status:
            typeof status === 'string'
              ? status
                  .toLowerCase()
                  .replace(/^enable$/, 'enabled')
                  .replace(/^disable$/, 'disabled')
              : status,
        },
      };
  }
  if (field === 'duplicateChannel' && variables.input)
    return { action: 'channel.duplicate_form', targetIDs: [String(variables.sourceID)], payload: object(variables.input) };
  if (field === 'bulkCreateModels')
    return { action: 'model.batch_form', targetIDs: [], payload: { models: variables.inputs ?? variables.input } };
  if (field === 'duplicateChannel')
    return {
      action: 'channel.duplicate',
      targetIDs: [String(variables.channelID ?? variables.id)],
      payload: {
        name: variables.name ?? object(variables.input).name,
        include_prices: Boolean(variables.includePrices ?? object(variables.input).includePrices),
      },
    };
  if (settings[field])
    return {
      action: 'system.setting',
      targetIDs: [],
      payload: {
        key: settings[field],
        value: snake(
          variables.input ??
            variables.settings ??
            variables.policy ??
            variables.config ??
            variables.name ??
            variables.title ??
            variables.logo
        ),
      },
    };
  return null;
}
export function requestGovernedMutation(query: string, variables?: Variables) {
  const command = governedMutation(query, variables);
  if (command && typeof window !== 'undefined')
    window.dispatchEvent(new CustomEvent<GovernanceCommand>(GOVERNANCE_REQUEST_EVENT, { detail: command }));
}
