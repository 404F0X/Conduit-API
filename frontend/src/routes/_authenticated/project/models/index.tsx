import { createFileRoute } from '@tanstack/react-router';
import ModelMarket from '@/features/model-market';

export const Route = createFileRoute('/_authenticated/project/models/')({ component: ModelMarket });
