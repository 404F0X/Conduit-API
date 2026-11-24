import { APP_ROUTES, NAV_SECTIONS } from '@/config/app-routes';
import { Command } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { useAuthStore } from '@/stores/authStore';
import { useRoutePermissions } from '@/hooks/useRoutePermissions';
import { useMe } from '@/features/auth/data/auth';
import { useProductExperience } from '@/features/product-experience';
import { type NavGroup, type NavLink, type SidebarData } from './components/layout/types';

export function useSidebarData(): SidebarData {
  const { t } = useTranslation();
  const { user: authUser } = useAuthStore((state) => state.auth);
  const { data: meData } = useMe();
  const { mode, features } = useProductExperience();
  const { filterNavGroups } = useRoutePermissions();
  const user = meData || authUser;

  const getInitials = (firstName?: string, lastName?: string, email?: string) => {
    if (firstName && lastName) return `${firstName.charAt(0)}${lastName.charAt(0)}`.toUpperCase();
    if (firstName) return firstName.slice(0, 2).toUpperCase();
    if (email) return email.split('@')[0].slice(0, 2).toUpperCase();
    return 'U';
  };

  const getDisplayName = (firstName?: string, lastName?: string, email?: string) => {
    if (firstName && lastName) return `${firstName} ${lastName}`;
    if (firstName) return firstName;
    if (email) {
      const username = email.split('@')[0];
      return username.charAt(0).toUpperCase() + username.slice(1);
    }
    return 'User';
  };

  // 侧边栏完全由路由注册表（config/app-routes.ts）生成：按模式取分组顺序，再把登记了 nav 的路由放进去。
  // 简单模式：「我的」六项所有人都有（包括站长），站长和管理员多一组「站点管理」。
  // 企业模式：工作区 / 运营 / 供应 / 系统 四组。
  const rawNavGroups: NavGroup[] = NAV_SECTIONS[mode].map((section) => ({
    title: t(section.titleKey),
    items: APP_ROUTES.flatMap((route) => {
      const placement = route.nav?.[mode];
      if (!placement || placement.section !== section.id) return [];
      return [{ route, placement }];
    })
      .sort((a, b) => a.placement.order - b.placement.order)
      .map(
        ({ route, placement }) =>
          ({
            title: t(placement.titleKey ?? route.titleKey),
            url: route.path,
            icon: route.icon,
            mobileOnly: placement.mobileOnly,
          }) as NavLink
      ),
  }));
  const featureRoutes: Record<string, string> = {
    '/operations': 'operations_reports',
    '/projects': 'projects',
    '/roles': 'roles',
    '/change-sets': 'change_sets',
    '/groups': 'groups',
    '/system': 'system',
    '/prompt-protection-rules': 'prompt_protection',
    '/data-storages': 'data_storages',
    '/project/prompts': 'prompts',
    '/project/traces': 'traces',
    '/project/threads': 'threads',
    '/project/playground': 'playground',
  };
  const visibility = (url?: string) => features.find((feature) => feature.id === featureRoutes[url ?? ''])?.visibility;
  const accessible = filterNavGroups(rawNavGroups);
  const advanced = accessible.flatMap((group) => group.items).filter((item) => visibility(item.url) === 'OFF');
  const filteredNavGroups = accessible
    .map((group) => ({ ...group, items: group.items.filter((item) => visibility(item.url) !== 'OFF') }))
    .filter((group) => group.items.length);
  if (advanced.length)
    filteredNavGroups.push({ title: t('productFeatures.catalog', { defaultValue: 'Feature catalog' }), items: advanced });

  return {
    user: {
      name: getDisplayName(user?.firstName, user?.lastName, user?.email),
      email: user?.email || 'user@example.com',
      avatar: user?.avatar || getInitials(user?.firstName, user?.lastName, user?.email),
    },
    teams: [
      {
        name: t('sidebar.team.name'),
        logo: Command,
        description: '',
      },
    ],
    navGroups: filteredNavGroups,
  };
}
