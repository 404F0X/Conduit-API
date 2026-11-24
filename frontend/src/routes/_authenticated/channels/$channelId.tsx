import { createFileRoute } from '@tanstack/react-router';
import { RouteGuard } from '@/components/route-guard';
import { ChannelDetail } from '@/features/channels/detail';

function ProtectedChannelDetail() {
  const { channelId } = Route.useParams();
  return (
    <RouteGuard requiredScopes={['read_channels']}>
      <ChannelDetail channelId={channelId} />
    </RouteGuard>
  );
}

export const Route = createFileRoute('/_authenticated/channels/$channelId')({
  component: ProtectedChannelDetail,
});
