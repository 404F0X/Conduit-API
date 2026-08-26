import { useMutation, useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query';
import { graphqlRequest } from '@/gql/graphql';

export type SimpleGroupStatus = 'ENABLED' | 'DISABLED' | 'ARCHIVED';

export type SimpleGroup = {
  id: string;
  name: string;
  description?: string | null;
  status: SimpleGroupStatus;
  isDefault: boolean;
  accessPlanID: string;
  priceTierID: string;
  defaultSubscriptionPlanID?: string | null;
  modelIDs: string[];
  routeIDs: string[];
  multiplierPpm: number;
  memberUserIDs: string[];
  memberProjectIDs: string[];
  unresolvedMemberCount: number;
  createdAt: string;
  updatedAt: string;
};

export type SimpleGroupCreateInput = {
  name: string;
  description?: string;
  isDefault: boolean;
  modelIDs: string[];
  routeIDs: string[];
  multiplierPpm: number;
  defaultSubscriptionPlanID?: string;
  userIDs?: string[];
};

export type SimpleGroupUpdateInput = {
  groupID: string;
  name?: string;
  description?: string;
  clearDescription?: boolean;
  status?: Exclude<SimpleGroupStatus, 'ARCHIVED'>;
  isDefault?: boolean;
  modelIDs?: string[];
  routeIDs?: string[];
  multiplierPpm?: number;
  defaultSubscriptionPlanID?: string;
  clearDefaultSubscriptionPlan?: boolean;
  userIDs?: string[];
};

export type GroupUserOption = {
  id: string;
  email: string;
  firstName: string;
  lastName: string;
};

export type GroupModelOption = {
  id: string;
  modelID: string;
  name: string;
};

export type GroupRouteOption = {
  id: string;
  publicModelKey: string;
  deploymentName: string;
  channelName: string;
  upstreamModelID: string;
  status: 'ENABLED' | 'DISABLED' | 'ARCHIVED';
};

type SimpleGroupsCatalog = {
  simpleGroups: SimpleGroup[];
};

type UsersCatalog = {
  users: { edges: { node: GroupUserOption }[] };
};

type ModelsCatalog = {
  models: { edges: { node: GroupModelOption }[] };
};

type RoutesCatalog = {
  modelRoutes: GroupRouteOption[];
};

const SIMPLE_GROUPS_QUERY = `
  query SimpleGroupsCatalog {
    simpleGroups {
      id
      name
      description
      status
      isDefault
      accessPlanID
      priceTierID
      defaultSubscriptionPlanID
      modelIDs
      routeIDs
      multiplierPpm
      memberUserIDs
      memberProjectIDs
      unresolvedMemberCount
      createdAt
      updatedAt
    }
  }
`;

const USERS_CATALOG_QUERY = `
  query SimpleGroupUsersCatalog {
    users(first: 500) {
      edges { node { id email firstName lastName } }
    }
  }
`;

const MODELS_CATALOG_QUERY = `
  query SimpleGroupModelsCatalog {
    models(first: 500) {
      edges { node { id modelID name } }
    }
  }
`;

const ROUTES_CATALOG_QUERY = `
  query SimpleGroupRoutesCatalog {
    modelRoutes {
      id publicModelKey deploymentName channelName upstreamModelID status
    }
  }
`;

const CREATE_SIMPLE_GROUP_MUTATION = `
  mutation CreateSimpleGroup($input: CreateSimpleGroupInput!) {
    createSimpleGroup(input: $input) { id }
  }
`;

const UPDATE_SIMPLE_GROUP_MUTATION = `
  mutation UpdateSimpleGroup($input: UpdateSimpleGroupInput!) {
    updateSimpleGroup(input: $input) { id }
  }
`;

const ARCHIVE_SIMPLE_GROUP_MUTATION = `
  mutation ArchiveSimpleGroup($id: ID!) {
    deleteSimpleGroup(id: $id) { id status }
  }
`;

export function useSimpleGroups() {
  return useQuery({
    queryKey: ['simple-groups'],
    queryFn: () => graphqlRequest<SimpleGroupsCatalog>(SIMPLE_GROUPS_QUERY),
  });
}

export function useSimpleGroupUsersCatalog(enabled: boolean) {
  return useQuery({
    queryKey: ['simple-groups', 'users-catalog'],
    queryFn: () => graphqlRequest<UsersCatalog>(USERS_CATALOG_QUERY),
    enabled,
  });
}

export function useSimpleGroupModelsCatalog(enabled: boolean) {
  return useQuery({
    queryKey: ['simple-groups', 'models-catalog'],
    queryFn: () => graphqlRequest<ModelsCatalog>(MODELS_CATALOG_QUERY),
    enabled,
  });
}

export function useSimpleGroupRoutesCatalog(enabled: boolean) {
  return useQuery({
    queryKey: ['simple-groups', 'routes-catalog'],
    queryFn: () => graphqlRequest<RoutesCatalog>(ROUTES_CATALOG_QUERY),
    enabled,
  });
}

export function useCreateSimpleGroup() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (input: SimpleGroupCreateInput) =>
      graphqlRequest<{ createSimpleGroup: { id: string } }>(CREATE_SIMPLE_GROUP_MUTATION, { input }),
    onSuccess: () => invalidateSimpleGroupConsumers(client),
  });
}

export function useUpdateSimpleGroup() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (input: SimpleGroupUpdateInput) =>
      graphqlRequest<{ updateSimpleGroup: { id: string } }>(UPDATE_SIMPLE_GROUP_MUTATION, { input }),
    onSuccess: () => invalidateSimpleGroupConsumers(client),
  });
}

export function useArchiveSimpleGroup() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (id: string) =>
      graphqlRequest<{ deleteSimpleGroup: { id: string; status: SimpleGroupStatus } }>(ARCHIVE_SIMPLE_GROUP_MUTATION, { id }),
    onSuccess: () => invalidateSimpleGroupConsumers(client),
  });
}

function invalidateSimpleGroupConsumers(client: QueryClient) {
  return Promise.all([
    client.invalidateQueries({ queryKey: ['simple-groups'] }),
    client.invalidateQueries({ queryKey: ['billing'] }),
    client.invalidateQueries({ queryKey: ['myModelCatalog'] }),
  ]);
}
