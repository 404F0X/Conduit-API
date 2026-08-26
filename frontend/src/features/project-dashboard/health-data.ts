import { useQuery } from '@tanstack/react-query';
import { graphqlRequest } from '@/gql/graphql';

export type PublicChannelHealth = {
  status: 'OPERATIONAL' | 'DEGRADED' | 'DISRUPTED' | 'UNKNOWN';
  successRate?: number | null;
  avgTimeToFirstTokenMs?: number | null;
  avgTokensPerSecond?: number | null;
  lastUpdatedAt?: string | null;
};

const PUBLIC_CHANNEL_HEALTH_QUERY = `
  query PublicChannelHealth {
    publicChannelHealth {
      status successRate avgTimeToFirstTokenMs avgTokensPerSecond lastUpdatedAt
    }
  }
`;

export function usePublicChannelHealth() {
  return useQuery({
    queryKey: ['publicChannelHealth'],
    queryFn: async () => {
      const data = await graphqlRequest<{ publicChannelHealth?: PublicChannelHealth | null }>(PUBLIC_CHANNEL_HEALTH_QUERY);
      return data.publicChannelHealth ?? null;
    },
    staleTime: 60_000,
    retry: false,
  });
}
