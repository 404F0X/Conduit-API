import {
  IconAB2,
  IconActivity,
  IconAi,
  IconBaselineDensityMedium,
  IconDatabase,
  IconKey,
  IconLayoutDashboard,
  IconNote,
  IconReportMoney,
  IconPackages,
  IconRobot,
  IconSettings,
  IconShield,
  IconUsers,
  IconUsersGroup,
  IconWallet,
} from '@tabler/icons-react';
import { ClipboardCheck, Command, History } from 'lucide-react';
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
  const isAdministrator =
    !!user?.isOwner ||
    !!user?.scopes?.some(
      (scope) => scope === '*' || ['read_dashboard', 'read_channels', 'read_users', 'read_settings', 'read_billing'].includes(scope)
    );

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

  const sharedNavGroups: NavGroup[] = [
    {
      title: t('sidebar.groups.settings'),
      items: [
        {
          title: t('sidebar.items.system'),
          url: '/system',
          icon: IconSettings,
          mobileOnly: true,
        } as NavLink,
      ],
    },
  ];

  const simpleNavGroups: NavGroup[] = isAdministrator
    ? [
        {
          title: t('sidebar.groups.admin'),
          items: [
            { title: t('sidebar.items.dashboard'), url: '/', icon: IconLayoutDashboard } as NavLink,
            { title: t('sidebar.items.channels'), url: '/channels', icon: IconAi } as NavLink,
            { title: t('sidebar.items.models'), url: '/models', icon: IconRobot } as NavLink,
            {
              title: `${t('sidebar.items.users')} / ${t('sidebar.items.groups')}`,
              icon: IconUsers,
              items: [
                { title: t('sidebar.items.users'), url: '/users' },
                { title: '用户组 / 模型组 / Project 规则', url: '/groups' },
              ],
            },
            { title: t('sidebar.items.billing'), url: '/billing', icon: IconWallet } as NavLink,
            { title: t('sidebar.items.requests'), url: '/project/requests', icon: IconActivity } as NavLink,
          ],
        },
      ]
    : [
        {
          title: t('sidebar.groups.project'),
          items: [
            { title: t('sidebar.items.dashboard'), url: '/project/dashboard', icon: IconLayoutDashboard } as NavLink,
            { title: t('sidebar.items.apiKeys'), url: '/project/api-keys', icon: IconKey } as NavLink,
            { title: t('sidebar.items.modelMarket'), url: '/project/models', icon: IconRobot } as NavLink,
            { title: t('sidebar.items.wallet'), url: '/project/wallet', icon: IconWallet } as NavLink,
            { title: t('sidebar.items.requests'), url: '/project/requests', icon: IconActivity } as NavLink,
          ],
        },
      ];

  const enterpriseNavGroups: NavGroup[] = [
    {
      title: t('sidebar.groups.admin'),
      items: [
        { title: t('sidebar.items.dashboard'), url: '/', icon: IconLayoutDashboard } as NavLink,
        { title: t('sidebar.items.operations'), url: '/operations', icon: IconReportMoney } as NavLink,
        { title: t('sidebar.items.projects'), url: '/projects', icon: IconPackages } as NavLink,
        { title: t('sidebar.items.channels'), url: '/channels', icon: IconAi } as NavLink,
        { title: t('sidebar.items.models'), url: '/models', icon: IconRobot } as NavLink,
        { title: t('sidebar.items.changeSets'), url: '/change-sets', icon: ClipboardCheck } as NavLink,
        { title: t('sidebar.items.changelog'), url: '/changelog', icon: History } as NavLink,
        {
          title: t('sidebar.items.promptProtectionRules'),
          url: '/prompt-protection-rules',
          icon: IconShield,
        } as NavLink,
        { title: t('sidebar.items.dataStorages'), url: '/data-storages', icon: IconDatabase } as NavLink,
        { title: t('sidebar.items.users'), url: '/users', icon: IconUsers } as NavLink,
        { title: t('sidebar.items.billing'), url: '/billing', icon: IconWallet } as NavLink,
        { title: t('sidebar.items.groups'), url: '/groups', icon: IconUsersGroup } as NavLink,
        { title: t('sidebar.items.roles'), url: '/roles', icon: IconShield } as NavLink,
      ],
    },
    {
      title: t('sidebar.groups.project'),
      items: [
        { title: t('sidebar.items.dashboard'), url: '/project/dashboard', icon: IconLayoutDashboard } as NavLink,
        { title: t('sidebar.items.apiKeys'), url: '/project/api-keys', icon: IconKey } as NavLink,
        { title: t('sidebar.items.modelMarket'), url: '/project/models', icon: IconRobot } as NavLink,
        { title: t('sidebar.items.wallet'), url: '/project/wallet', icon: IconWallet } as NavLink,
        { title: t('sidebar.items.prompts'), url: '/project/prompts', icon: IconNote } as NavLink,
        { title: t('sidebar.items.requests'), url: '/project/requests', icon: IconActivity } as NavLink,
        { title: t('sidebar.items.traces'), url: '/project/traces', icon: IconAB2 } as NavLink,
        { title: t('sidebar.items.threads'), url: '/project/threads', icon: IconBaselineDensityMedium } as NavLink,
        { title: t('sidebar.items.users'), url: '/project/users', icon: IconUsers } as NavLink,
        { title: t('sidebar.items.roles'), url: '/project/roles', icon: IconShield } as NavLink,
        { title: t('sidebar.items.playground'), url: '/project/playground', icon: IconRobot } as NavLink,
      ],
    },
  ];

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
  const isHidden = (url?: string) => !!url && features.find((feature) => feature.id === featureRoutes[url])?.visibility === 'OFF';
  const primaryNavGroups = simpleNavGroups.map((group) => ({
    ...group,
    items: group.items
      .filter((item) => !isHidden(item.url))
      .map((item) => (item.items ? { ...item, items: item.items.filter((child) => !isHidden(child.url)) } : item))
      .filter((item) => !item.items || item.items.length > 0),
  }));
  const primaryPaths = new Set(
    primaryNavGroups.flatMap((group) => group.items.flatMap((item) => item.items?.map((child) => child.url) ?? [item.url]))
  );
  const advanced = filterNavGroups([...enterpriseNavGroups, ...sharedNavGroups])
    .flatMap((group) => group.items)
    .filter((item): item is NavLink => !!item.url && !primaryPaths.has(item.url));
  const pinned = advanced.filter(
    (item) => features.find((feature) => feature.id === featureRoutes[item.url as string])?.visibility === 'ON'
  );
  const hidden = advanced.filter((item) => !pinned.includes(item));
  const rawNavGroups: NavGroup[] =
    mode === 'SIMPLE'
      ? [
          ...primaryNavGroups,
          ...(pinned.length ? [{ title: t('productFeatures.pinned', { defaultValue: 'Pinned features' }), items: pinned }] : []),
          {
            title: t('productFeatures.catalog', { defaultValue: 'Feature catalog' }),
            items: [{ title: t('productFeatures.advanced', { defaultValue: 'Advanced features' }), icon: IconSettings, items: hidden }],
          },
        ]
      : [
          ...[...enterpriseNavGroups, ...sharedNavGroups].map((group) => ({
            ...group,
            items: group.items.filter((item) => !isHidden(item.url)),
          })),
          ...(advanced.some((item) => isHidden(item.url))
            ? [
                {
                  title: t('productFeatures.catalog', { defaultValue: 'Feature catalog' }),
                  items: [
                    {
                      title: t('productFeatures.advanced', { defaultValue: 'Advanced features' }),
                      icon: IconSettings,
                      items: advanced.filter((item) => isHidden(item.url)),
                    },
                  ],
                },
              ]
            : []),
        ];
  const filteredNavGroups = filterNavGroups(rawNavGroups).filter((group) => group.items.length > 0);

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
