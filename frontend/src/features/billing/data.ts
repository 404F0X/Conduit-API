import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { graphqlRequest } from '@/gql/graphql';
import { useSelectedProjectId } from '@/stores/projectStore';

export type CreditLedgerEntry = { id: string; amount: string; entryType: string; description?: string | null; createdAt: string };
export type UserBalance = {
  userID: string;
  currency: string;
  creditBalance: string;
  subscriptionBalance: string;
  generalSubscriptionBalance?: string;
  dedicatedSubscriptionBalance?: string;
  reservedBalance: string;
  availableBalance: string;
  ledgerEntries: CreditLedgerEntry[];
};
export type ProjectBalance = {
  projectID: string;
  currency: string;
  walletStatus: string;
  creditBalance: string;
  subscriptionBalance: string;
  generalSubscriptionBalance?: string;
  dedicatedSubscriptionBalance?: string;
  reservedBalance: string;
  availableBalance: string;
  ledgerEntries: CreditLedgerEntry[];
};
export type ProjectWalletComparison = {
  projectID: string;
  ownerUserID: string;
  status: string;
  legacyCreditBalance: string;
  projectCreditBalance: string;
  legacySubscriptionBalance: string;
  projectSubscriptionBalance: string;
  legacyAvailableBalance: string;
  projectAvailableBalance: string;
  availableDelta: string;
  generatedAt: string;
};
export type SubscriptionAccessPlan = {
  id: string;
  name: string;
};
export type QuotaClass = 'GENERAL' | 'DEDICATED';
export type SubscriptionQuotaRule = {
  id: string;
  name: string;
  quotaClass: QuotaClass;
  allowance: string;
  rolloverMode: 'NONE' | 'CAPPED';
  rolloverCap?: string | null;
  carryoverDays?: number | null;
  accessPlans: SubscriptionAccessPlan[];
};
export type SubscriptionQuotaRuleInput = {
  id?: string;
  name: string;
  quotaClass: QuotaClass;
  allowance: string;
  rolloverMode: 'NONE' | 'CAPPED';
  rolloverCap?: string;
  carryoverDays?: number;
  accessPlanIDs: string[];
};
export type SubscriptionAllowanceBucket = {
  id: string;
  name: string;
  quotaClass: QuotaClass;
  sourceType: string;
  periodStart: string;
  periodEnd: string;
  expiresAt: string;
  grantedAllowance: string;
  consumedAllowance: string;
  reservedAllowance: string;
  remainingAllowance: string;
  status: string;
  accessPlans: SubscriptionAccessPlan[];
  modelIDs: string[];
  sourceBucketID?: string | null;
};
export type SubscriptionPlan = {
  id: string;
  name: string;
  /** @deprecated Read-only compatibility field for plans created before quota rules. */
  currency?: string;
  /** @deprecated Read-only compatibility field for plans created before quota rules. */
  allowance?: string;
  intervalUnit: 'DAY' | 'MONTH' | 'YEAR';
  intervalCount: number;
  /** @deprecated Read-only compatibility field for plans created before quota rules. */
  rolloverMode?: 'NONE' | 'CAPPED';
  /** @deprecated Read-only compatibility field for plans created before quota rules. */
  rolloverCap?: string | null;
  status: 'ENABLED' | 'DISABLED' | 'ARCHIVED';
  quotaRules: SubscriptionQuotaRule[];
  /** Access permission grants. Funding scopes live on quotaRules instead. */
  accessPlans: SubscriptionAccessPlan[];
};
export type UserSubscription = {
  id: string;
  userID: string;
  plan: SubscriptionPlan;
  status: string;
  currentPeriodStart: string;
  currentPeriodEnd: string;
  autoRenew: boolean;
  intervalUnit: SubscriptionPlan['intervalUnit'];
  intervalCount: number;
  projectID?: string | null;
  grantedAccessPlans: SubscriptionAccessPlan[];
  grantedGroupNames: string[];
  grantedModelIDs: string[];
  grantedAllowance: string;
  consumedAllowance: string;
  reservedAllowance: string;
  remainingAllowance: string;
  generalRemainingAllowance?: string;
  dedicatedRemainingAllowance?: string;
  allowanceBuckets: SubscriptionAllowanceBucket[];
};
export type GrantUserCreditInput = {
  userID: string;
  amount: string;
  description?: string;
  idempotencyKey: string;
};
export type GrantProjectCreditInput = {
  projectID: string;
  amount: string;
  description?: string;
  idempotencyKey: string;
};
export type CreateSubscriptionPlanInput = {
  name: string;
  intervalUnit: SubscriptionPlan['intervalUnit'];
  intervalCount?: number;
  accessPlanIDs: string[];
  quotaRules: SubscriptionQuotaRuleInput[];
};
export type UpdateSubscriptionPlanInput = {
  id: string;
  name: string;
  intervalUnit: SubscriptionPlan['intervalUnit'];
  intervalCount: number;
  accessPlanIDs: string[];
  quotaRules: SubscriptionQuotaRuleInput[];
  status: SubscriptionPlan['status'];
};

export type BillingAccessBundle = {
  id: string;
  name: string;
  status: 'ENABLED' | 'DISABLED' | 'ARCHIVED';
  accessPlanID: string;
};

export type SubscriptionProject = {
  id: string;
  name: string;
  status: string;
  commercialPolicyActive: boolean;
};

const QUOTA_RULE_FIELDS = `id name quotaClass allowance rolloverMode rolloverCap carryoverDays accessPlans { id name }`;
const ALLOWANCE_BUCKET_FIELDS = `id name quotaClass sourceType periodStart periodEnd expiresAt grantedAllowance consumedAllowance reservedAllowance remainingAllowance status accessPlans { id name } modelIDs sourceBucketID`;
const PLAN_FIELDS = `id name currency allowance intervalUnit intervalCount rolloverMode rolloverCap status quotaRules { ${QUOTA_RULE_FIELDS} } accessPlans { id name }`;
const SUBSCRIPTION_FIELDS = `id userID projectID status currentPeriodStart currentPeriodEnd autoRenew intervalUnit intervalCount grantedAccessPlans { id name } grantedGroupNames grantedModelIDs grantedAllowance consumedAllowance reservedAllowance remainingAllowance generalRemainingAllowance dedicatedRemainingAllowance allowanceBuckets { ${ALLOWANCE_BUCKET_FIELDS} } plan { ${PLAN_FIELDS} }`;
const BALANCE_FIELDS = `userID currency creditBalance subscriptionBalance generalSubscriptionBalance dedicatedSubscriptionBalance reservedBalance availableBalance ledgerEntries { id amount entryType description createdAt }`;
const PROJECT_BALANCE_FIELDS = `projectID currency walletStatus creditBalance subscriptionBalance generalSubscriptionBalance dedicatedSubscriptionBalance reservedBalance availableBalance ledgerEntries { id amount entryType description createdAt }`;
const PROJECT_COMPARISON_FIELDS = `projectID ownerUserID status legacyCreditBalance projectCreditBalance legacySubscriptionBalance projectSubscriptionBalance legacyAvailableBalance projectAvailableBalance availableDelta generatedAt`;

const BALANCE_QUERY = `query UserBalance($userID: ID!) { userBalance(userID: $userID) { ${BALANCE_FIELDS} } }`;
const PLANS_QUERY = `query SubscriptionPlans { subscriptionPlans { ${PLAN_FIELDS} } }`;
const BILLING_ACCESS_BUNDLES_QUERY = `query BillingAccessBundles { simpleGroups { id name status accessPlanID } }`;
const SUBSCRIPTIONS_QUERY = `query UserSubscriptions($userID: ID!) { userSubscriptions(userID: $userID) { ${SUBSCRIPTION_FIELDS} } }`;
const SUBSCRIPTION_PROJECTS_QUERY = `query SubscriptionProjects($userID: ID!) { subscriptionProjects(userID: $userID) { id name status commercialPolicyActive } }`;
const MY_BALANCE_QUERY = `query MyBalance { myBalance { ${PROJECT_BALANCE_FIELDS} } }`;
const MY_SUBSCRIPTIONS_QUERY = `query MySubscriptions { mySubscriptions { ${SUBSCRIPTION_FIELDS} } }`;
const PROJECT_BALANCE_QUERY = `query ProjectBalance($projectID: ID!) { projectBalance(projectID: $projectID) { ${PROJECT_BALANCE_FIELDS} } }`;
const PROJECT_WALLET_COMPARISON_QUERY = `query ProjectWalletComparison($projectID: ID!) { projectWalletComparison(projectID: $projectID) { ${PROJECT_COMPARISON_FIELDS} } }`;
const MY_PROJECT_BALANCE_QUERY = `query MyProjectBalance { myProjectBalance { ${PROJECT_BALANCE_FIELDS} } }`;
const MY_PROJECT_WALLET_COMPARISON_QUERY = `query MyProjectWalletComparison { myProjectWalletComparison { ${PROJECT_COMPARISON_FIELDS} } }`;
const GRANT_CREDIT_MUTATION = `mutation GrantUserCredit($input: GrantUserCreditInput!) { grantUserCredit(input: $input) { ${BALANCE_FIELDS} } }`;
const GRANT_PROJECT_CREDIT_MUTATION = `mutation GrantProjectCredit($input: GrantProjectCreditInput!) { grantProjectCredit(input: $input) { ${PROJECT_BALANCE_FIELDS} } }`;
const CREATE_PLAN_MUTATION = `mutation CreateSubscriptionPlan($input: CreateSubscriptionPlanInput!) { createSubscriptionPlan(input: $input) { ${PLAN_FIELDS} } }`;
const UPDATE_PLAN_MUTATION = `mutation UpdateSubscriptionPlan($input: UpdateSubscriptionPlanInput!) { updateSubscriptionPlan(input: $input) { ${PLAN_FIELDS} } }`;
const ASSIGN_SUBSCRIPTION_MUTATION = `mutation AssignUserSubscription($input: AssignUserSubscriptionInput!) { assignUserSubscription(input: $input) { ${SUBSCRIPTION_FIELDS} } }`;
const REFRESH_ALLOWANCE_MUTATION = `mutation RefreshSubscriptionAllowance($subscriptionID: ID!) { refreshSubscriptionAllowance(subscriptionID: $subscriptionID) { ${SUBSCRIPTION_FIELDS} } }`;
const PAUSE_SUBSCRIPTION_MUTATION = `mutation PauseUserSubscription($subscriptionID: ID!) { pauseUserSubscription(subscriptionID: $subscriptionID) { ${SUBSCRIPTION_FIELDS} } }`;
const RESUME_SUBSCRIPTION_MUTATION = `mutation ResumeUserSubscription($subscriptionID: ID!) { resumeUserSubscription(subscriptionID: $subscriptionID) { ${SUBSCRIPTION_FIELDS} } }`;
const CANCEL_SUBSCRIPTION_MUTATION = `mutation CancelUserSubscription($subscriptionID: ID!) { cancelUserSubscription(subscriptionID: $subscriptionID) { ${SUBSCRIPTION_FIELDS} } }`;
const RENEW_SUBSCRIPTION_MUTATION = `mutation RenewUserSubscription($subscriptionID: ID!) { renewUserSubscription(subscriptionID: $subscriptionID) { ${SUBSCRIPTION_FIELDS} } }`;
const SET_SUBSCRIPTION_AUTO_RENEW_MUTATION = `mutation SetSubscriptionAutoRenew($input: SetSubscriptionAutoRenewInput!) { setSubscriptionAutoRenew(input: $input) { ${SUBSCRIPTION_FIELDS} } }`;

export function useUserBalance(userID?: string, enabled = true) {
  return useQuery({
    queryKey: ['billing', 'balance', userID],
    queryFn: () => graphqlRequest<{ userBalance: UserBalance }>(BALANCE_QUERY, { userID }),
    enabled: enabled && !!userID,
  });
}
export function useSubscriptionPlans(enabled = true) {
  return useQuery({
    queryKey: ['billing', 'plans'],
    queryFn: () => graphqlRequest<{ subscriptionPlans: SubscriptionPlan[] }>(PLANS_QUERY),
    enabled,
  });
}
export function useBillingAccessBundles(enabled = true) {
  return useQuery({
    queryKey: ['billing', 'access-bundles'],
    queryFn: () => graphqlRequest<{ simpleGroups: BillingAccessBundle[] }>(BILLING_ACCESS_BUNDLES_QUERY),
    enabled,
  });
}
export function useUserSubscriptions(userID?: string, enabled = true) {
  return useQuery({
    queryKey: ['billing', 'subscriptions', userID],
    queryFn: () => graphqlRequest<{ userSubscriptions: UserSubscription[] }>(SUBSCRIPTIONS_QUERY, { userID }),
    enabled: enabled && !!userID,
  });
}
export function useSubscriptionProjects(userID?: string) {
  return useQuery({
    queryKey: ['billing', 'subscription-projects', userID],
    queryFn: () => graphqlRequest<{ subscriptionProjects: SubscriptionProject[] }>(SUBSCRIPTION_PROJECTS_QUERY, { userID }),
    enabled: !!userID,
  });
}
export function useMyBalance() {
  const selectedProjectID = useSelectedProjectId();
  return useQuery({
    queryKey: ['billing', 'my-balance', selectedProjectID],
    queryFn: () => graphqlRequest<{ myBalance: ProjectBalance }>(MY_BALANCE_QUERY, undefined, { 'X-Project-ID': selectedProjectID! }),
    enabled: !!selectedProjectID,
  });
}
export function useMySubscriptions() {
  const selectedProjectID = useSelectedProjectId();
  return useQuery({
    queryKey: ['billing', 'my-subscriptions', selectedProjectID],
    queryFn: () =>
      graphqlRequest<{ mySubscriptions: UserSubscription[] }>(MY_SUBSCRIPTIONS_QUERY, undefined, {
        'X-Project-ID': selectedProjectID!,
      }),
    enabled: !!selectedProjectID,
  });
}
export function useProjectBalance(projectID?: string, enabled = true) {
  return useQuery({
    queryKey: ['billing', 'project-balance', projectID],
    queryFn: () => graphqlRequest<{ projectBalance: ProjectBalance | null }>(PROJECT_BALANCE_QUERY, { projectID }),
    enabled: enabled && !!projectID,
  });
}
export function useProjectWalletComparison(projectID?: string) {
  return useQuery({
    queryKey: ['billing', 'project-wallet-comparison', projectID],
    queryFn: () =>
      graphqlRequest<{ projectWalletComparison: ProjectWalletComparison | null }>(PROJECT_WALLET_COMPARISON_QUERY, { projectID }),
    enabled: !!projectID,
  });
}
export function useMyProjectBalance() {
  const selectedProjectID = useSelectedProjectId();
  return useQuery({
    queryKey: ['billing', 'my-project-balance', selectedProjectID],
    queryFn: () =>
      graphqlRequest<{ myProjectBalance: ProjectBalance | null }>(MY_PROJECT_BALANCE_QUERY, undefined, {
        'X-Project-ID': selectedProjectID!,
      }),
    enabled: !!selectedProjectID,
  });
}
export function useMyProjectWalletComparison() {
  const selectedProjectID = useSelectedProjectId();
  return useQuery({
    queryKey: ['billing', 'my-project-wallet-comparison', selectedProjectID],
    queryFn: () =>
      graphqlRequest<{ myProjectWalletComparison: ProjectWalletComparison | null }>(MY_PROJECT_WALLET_COMPARISON_QUERY, undefined, {
        'X-Project-ID': selectedProjectID!,
      }),
    enabled: !!selectedProjectID,
  });
}

function useInvalidateUserBilling() {
  const client = useQueryClient();
  return (userID: string, projectID?: string | null) =>
    Promise.all([
      client.invalidateQueries({ queryKey: ['billing', 'balance', userID] }),
      client.invalidateQueries({ queryKey: ['billing', 'subscriptions', userID] }),
      client.invalidateQueries({ queryKey: ['billing', 'my-balance'] }),
      client.invalidateQueries({ queryKey: ['billing', 'my-subscriptions'] }),
      client.invalidateQueries({ queryKey: ['billing', 'my-project-balance'] }),
      client.invalidateQueries({ queryKey: ['billing', 'my-project-wallet-comparison'] }),
      ...(projectID
        ? [
            client.invalidateQueries({ queryKey: ['billing', 'project-balance', projectID] }),
            client.invalidateQueries({ queryKey: ['billing', 'project-wallet-comparison', projectID] }),
          ]
        : []),
    ]);
}
export function useGrantUserCredit() {
  const invalidate = useInvalidateUserBilling();
  return useMutation({
    mutationFn: (input: GrantUserCreditInput) => graphqlRequest<{ grantUserCredit: UserBalance }>(GRANT_CREDIT_MUTATION, { input }),
    onSuccess: (_data, input) => invalidate(input.userID),
  });
}
export function useGrantProjectCredit() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (input: GrantProjectCreditInput) =>
      graphqlRequest<{ grantProjectCredit: ProjectBalance }>(GRANT_PROJECT_CREDIT_MUTATION, { input }),
    onSuccess: (_data, input) =>
      Promise.all([
        client.invalidateQueries({ queryKey: ['billing', 'project-balance', input.projectID] }),
        client.invalidateQueries({ queryKey: ['billing', 'project-wallet-comparison', input.projectID] }),
        client.invalidateQueries({ queryKey: ['billing', 'my-project-balance'] }),
        client.invalidateQueries({ queryKey: ['billing', 'my-project-wallet-comparison'] }),
      ]),
  });
}
export function useCreateSubscriptionPlan() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (input: CreateSubscriptionPlanInput) =>
      graphqlRequest<{ createSubscriptionPlan: SubscriptionPlan }>(CREATE_PLAN_MUTATION, { input }),
    onSuccess: () => client.invalidateQueries({ queryKey: ['billing', 'plans'] }),
  });
}
export function useUpdateSubscriptionPlan() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (input: UpdateSubscriptionPlanInput) =>
      graphqlRequest<{ updateSubscriptionPlan: SubscriptionPlan }>(UPDATE_PLAN_MUTATION, { input }),
    onSuccess: () => client.invalidateQueries({ queryKey: ['billing', 'plans'] }),
  });
}
export function useAssignUserSubscription() {
  const invalidate = useInvalidateUserBilling();
  return useMutation({
    mutationFn: (input: {
      userID: string;
      planID: string;
      idempotencyKey: string;
      autoRenew?: boolean;
      intervalUnit?: SubscriptionPlan['intervalUnit'];
      intervalCount?: number;
      projectID: string;
    }) => graphqlRequest<{ assignUserSubscription: UserSubscription }>(ASSIGN_SUBSCRIPTION_MUTATION, { input }),
    onSuccess: (data, input) => invalidate(input.userID, data.assignUserSubscription.projectID || input.projectID),
  });
}
export function useRefreshSubscriptionAllowance() {
  const invalidate = useInvalidateUserBilling();
  return useMutation({
    mutationFn: ({ subscriptionID }: { subscriptionID: string; userID: string }) =>
      graphqlRequest<{ refreshSubscriptionAllowance: UserSubscription }>(REFRESH_ALLOWANCE_MUTATION, { subscriptionID }),
    onSuccess: (data, input) => invalidate(input.userID, data.refreshSubscriptionAllowance.projectID),
  });
}

export type SubscriptionLifecycleAction = 'pause' | 'resume' | 'cancel' | 'renew';

export function useSubscriptionLifecycle() {
  const invalidate = useInvalidateUserBilling();
  return useMutation({
    mutationFn: async ({ action, subscriptionID }: { action: SubscriptionLifecycleAction; subscriptionID: string; userID: string }) => {
      const operations = {
        pause: [PAUSE_SUBSCRIPTION_MUTATION, 'pauseUserSubscription'],
        resume: [RESUME_SUBSCRIPTION_MUTATION, 'resumeUserSubscription'],
        cancel: [CANCEL_SUBSCRIPTION_MUTATION, 'cancelUserSubscription'],
        renew: [RENEW_SUBSCRIPTION_MUTATION, 'renewUserSubscription'],
      } as const;
      const [query, field] = operations[action];
      const result = await graphqlRequest<Record<string, UserSubscription>>(query, { subscriptionID });
      return result[field];
    },
    onSuccess: (data, input) => invalidate(input.userID, data?.projectID),
  });
}

export function useSetSubscriptionAutoRenew() {
  const invalidate = useInvalidateUserBilling();
  return useMutation({
    mutationFn: ({ subscriptionID, autoRenew }: { subscriptionID: string; autoRenew: boolean; userID: string }) =>
      graphqlRequest<{ setSubscriptionAutoRenew: UserSubscription }>(SET_SUBSCRIPTION_AUTO_RENEW_MUTATION, {
        input: { subscriptionID, autoRenew },
      }),
    onSuccess: (_data, input) => invalidate(input.userID),
  });
}
