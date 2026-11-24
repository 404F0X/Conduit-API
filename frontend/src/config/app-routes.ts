import {
  IconAB2,
  IconActivity,
  IconAi,
  IconBaselineDensityMedium,
  IconDatabase,
  IconHome,
  IconKey,
  IconLayoutDashboard,
  IconListDetails,
  IconNote,
  IconPackages,
  IconReportMoney,
  IconRobot,
  IconSettings,
  IconShield,
  IconTicket,
  IconUserCircle,
  IconUsers,
  IconUsersGroup,
  IconWallet,
} from '@tabler/icons-react';
import { ClipboardCheck, History } from 'lucide-react';
import type { ScopeLevel, ScopeMatch } from '@/lib/permission-core';
import type { ProductMode } from '@/features/product-experience/mode';

/**
 * 路由注册表：每个页面只在这里登记一次。
 *
 * 侧边栏（sidebar.ts）、导航过滤（useRoutePermissions）、路由守卫（RouteGuard）
 * 和 config/route-permission.ts 都从这张表派生，不再各自维护一份列表。
 *
 * - modes：这个页面在哪种产品模式下存在；不写表示两种模式都有。
 * - nav：在哪种模式下出现在侧边栏的哪一组、排第几。同一个页面在两种模式下可以放在不同的组、用不同的名字。
 * - requiredScopes：没有这些权限时，侧边栏不显示，直接打开地址时显示无权限。
 */

export type NavSectionId =
  // 简单模式
  | 'me'
  | 'site'
  // 企业模式
  | 'workspace'
  | 'operations'
  | 'supply'
  | 'system'
  // 两种模式共用（只在手机端出现的入口等）
  | 'settings';

export interface NavPlacement {
  section: NavSectionId;
  order: number;
  /** 侧边栏上的名字（i18n key），不写则用路由的 titleKey */
  titleKey?: string;
  mobileOnly?: boolean;
}

export interface AppRoute {
  path: string;
  titleKey: string;
  icon?: React.ElementType;
  modes?: ProductMode[];
  requiredScopes?: string[];
  /** 某个模式下需要的权限和默认不同时写这里，例如企业模式的工作区概览要 read_requests */
  modeScopes?: Partial<Record<ProductMode, string[]>>;
  scopeMatch?: ScopeMatch;
  scopeLevel?: ScopeLevel;
  /** 没有权限时在侧边栏里隐藏还是置灰；默认隐藏 */
  deniedBehavior?: 'hidden' | 'disabled';
  nav?: Partial<Record<ProductMode, NavPlacement>>;
}

export interface NavSection {
  id: NavSectionId;
  titleKey: string;
}

/** 每种模式下侧边栏分组的先后顺序 */
export const NAV_SECTIONS: Record<ProductMode, NavSection[]> = {
  SIMPLE: [
    { id: 'me', titleKey: 'nav.sections.me' },
    { id: 'site', titleKey: 'nav.sections.site' },
    { id: 'settings', titleKey: 'sidebar.groups.settings' },
  ],
  ENTERPRISE: [
    { id: 'workspace', titleKey: 'nav.sections.workspace' },
    { id: 'operations', titleKey: 'nav.sections.operations' },
    { id: 'supply', titleKey: 'nav.sections.supply' },
    { id: 'system', titleKey: 'nav.sections.system' },
    { id: 'settings', titleKey: 'sidebar.groups.settings' },
  ],
};

export const GOVERNANCE_ROUTE_SCOPES = [
  'read_commercialization',
  'write_commercialization',
  'read_channels',
  'write_channels',
  'read_users',
  'write_users',
  'read_roles',
  'write_roles',
  'read_api_keys',
  'write_api_keys',
  'read_projects',
  'write_projects',
  'read_groups',
  'write_groups',
  'read_settings',
  'write_settings',
  'read_billing',
  'write_billing',
  'read_subscriptions',
  'write_subscriptions',
  'read_data_storages',
  'write_data_storages',
  'grant_credit',
  'review_changes',
  'publish_changes',
  'manage_governance',
];

const SYSTEM: ScopeLevel = 'system';

export const APP_ROUTES: AppRoute[] = [
  // ───────────── 个人 / 工作区（项目级，两种模式都有） ─────────────
  {
    path: '/project/dashboard',
    titleKey: 'nav.items.home',
    icon: IconHome,
    // 简单模式首页对所有登录用户开放，没有 read_requests 时页面里不显示请求相关的卡片；
    // 企业模式的工作区概览和以前一样要 read_requests
    modeScopes: { ENTERPRISE: ['read_requests'] },
    nav: {
      SIMPLE: { section: 'me', order: 10, titleKey: 'nav.items.home' },
      ENTERPRISE: { section: 'workspace', order: 10, titleKey: 'nav.items.workspaceOverview' },
    },
  },
  {
    path: '/project/api-keys',
    titleKey: 'nav.items.tokens',
    icon: IconKey,
    requiredScopes: ['read_api_keys'],
    nav: {
      SIMPLE: { section: 'me', order: 20, titleKey: 'nav.items.tokens' },
      ENTERPRISE: { section: 'workspace', order: 20, titleKey: 'sidebar.items.apiKeys' },
    },
  },
  {
    path: '/project/models',
    titleKey: 'nav.items.modelsAndPricing',
    icon: IconRobot,
    nav: {
      SIMPLE: { section: 'me', order: 30, titleKey: 'nav.items.modelsAndPricing' },
      ENTERPRISE: { section: 'workspace', order: 30, titleKey: 'sidebar.items.modelMarket' },
    },
  },
  {
    path: '/project/requests',
    titleKey: 'nav.items.usageLogs',
    icon: IconActivity,
    requiredScopes: ['read_requests'],
    nav: {
      SIMPLE: { section: 'me', order: 40, titleKey: 'nav.items.usageLogs' },
      ENTERPRISE: { section: 'workspace', order: 50, titleKey: 'nav.items.requestLogs' },
    },
  },
  {
    path: '/project/wallet',
    titleKey: 'nav.items.wallet',
    icon: IconWallet,
    nav: {
      SIMPLE: { section: 'me', order: 50, titleKey: 'nav.items.wallet' },
      ENTERPRISE: { section: 'workspace', order: 40, titleKey: 'nav.items.usageAndBilling' },
    },
  },
  {
    path: '/settings',
    titleKey: 'nav.items.settings',
    icon: IconUserCircle,
    nav: {
      SIMPLE: { section: 'me', order: 60, titleKey: 'nav.items.settings' },
    },
  },
  {
    path: '/project/users',
    titleKey: 'nav.items.members',
    icon: IconUsers,
    modes: ['ENTERPRISE'],
    requiredScopes: ['read_users'],
    nav: { ENTERPRISE: { section: 'workspace', order: 60, titleKey: 'nav.items.members' } },
  },
  {
    path: '/project/roles',
    titleKey: 'nav.items.projectRoles',
    icon: IconShield,
    modes: ['ENTERPRISE'],
    requiredScopes: ['read_roles'],
    nav: { ENTERPRISE: { section: 'workspace', order: 70, titleKey: 'nav.items.projectRoles' } },
  },
  {
    path: '/project/playground',
    titleKey: 'sidebar.items.playground',
    icon: IconRobot,
    modes: ['ENTERPRISE'],
    // 测试场会列出上游渠道和模型，只给能读渠道的人
    requiredScopes: ['read_channels'],
    nav: { ENTERPRISE: { section: 'workspace', order: 80 } },
  },
  {
    path: '/project/prompts',
    titleKey: 'sidebar.items.prompts',
    icon: IconNote,
    modes: ['ENTERPRISE'],
    requiredScopes: ['read_prompts'],
    nav: { ENTERPRISE: { section: 'workspace', order: 90 } },
  },
  {
    path: '/project/traces',
    titleKey: 'sidebar.items.traces',
    icon: IconAB2,
    modes: ['ENTERPRISE'],
    requiredScopes: ['read_requests'],
    nav: { ENTERPRISE: { section: 'workspace', order: 100 } },
  },
  {
    path: '/project/threads',
    titleKey: 'sidebar.items.threads',
    icon: IconBaselineDensityMedium,
    modes: ['ENTERPRISE'],
    requiredScopes: ['read_requests'],
    nav: { ENTERPRISE: { section: 'workspace', order: 110 } },
  },
  {
    path: '/project/usage-logs',
    titleKey: 'sidebar.items.usageLogs',
    modes: ['ENTERPRISE'],
    requiredScopes: ['read_requests'],
  },

  // ───────────── 站点管理（系统级） ─────────────
  {
    path: '/',
    titleKey: 'nav.items.siteOverview',
    icon: IconLayoutDashboard,
    requiredScopes: ['read_dashboard'],
    scopeLevel: SYSTEM,
    nav: {
      SIMPLE: { section: 'site', order: 10, titleKey: 'nav.items.siteOverview' },
      ENTERPRISE: { section: 'system', order: 10, titleKey: 'sidebar.items.dashboard' },
    },
  },
  {
    path: '/operations',
    titleKey: 'nav.items.businessOverview',
    icon: IconReportMoney,
    // 简单模式的“概览”已经包含经营数字，运营账本只留给企业模式
    modes: ['ENTERPRISE'],
    requiredScopes: ['read_dashboard'],
    scopeLevel: SYSTEM,
    nav: { ENTERPRISE: { section: 'operations', order: 10, titleKey: 'nav.items.businessOverview' } },
  },
  {
    path: '/users',
    titleKey: 'sidebar.items.users',
    icon: IconUsers,
    requiredScopes: ['read_users'],
    scopeLevel: SYSTEM,
    nav: {
      SIMPLE: { section: 'site', order: 20, titleKey: 'sidebar.items.users' },
      ENTERPRISE: { section: 'operations', order: 20, titleKey: 'nav.items.customers' },
    },
  },
  {
    path: '/groups',
    titleKey: 'nav.items.groupsAndRates',
    icon: IconUsersGroup,
    requiredScopes: ['read_groups'],
    scopeLevel: SYSTEM,
    nav: {
      SIMPLE: { section: 'site', order: 30, titleKey: 'nav.items.groupsAndRates' },
      ENTERPRISE: { section: 'supply', order: 40, titleKey: 'nav.items.groupsAndRates' },
    },
  },
  {
    path: '/channels',
    titleKey: 'sidebar.items.channels',
    icon: IconAi,
    requiredScopes: ['read_channels'],
    scopeLevel: SYSTEM,
    nav: {
      SIMPLE: { section: 'site', order: 40, titleKey: 'nav.items.channels' },
      ENTERPRISE: { section: 'supply', order: 10, titleKey: 'nav.items.channels' },
    },
  },
  {
    path: '/models',
    titleKey: 'nav.items.modelPricing',
    icon: IconRobot,
    requiredScopes: ['read_channels'],
    scopeLevel: SYSTEM,
    nav: {
      SIMPLE: { section: 'site', order: 50, titleKey: 'nav.items.modelPricing' },
      ENTERPRISE: { section: 'supply', order: 20, titleKey: 'nav.items.modelsAndRoutes' },
    },
  },
  {
    path: '/billing',
    titleKey: 'nav.items.plansAndCodes',
    icon: IconTicket,
    requiredScopes: ['read_billing', 'read_subscriptions'],
    scopeMatch: 'any',
    scopeLevel: SYSTEM,
    nav: {
      SIMPLE: { section: 'site', order: 60, titleKey: 'nav.items.plansAndCodes' },
      ENTERPRISE: { section: 'operations', order: 30, titleKey: 'nav.items.plans' },
    },
  },
  {
    path: '/requests',
    titleKey: 'nav.items.siteLogs',
    icon: IconListDetails,
    requiredScopes: ['read_requests'],
    scopeLevel: SYSTEM,
    // 全站请求日志：目前只有单条详情页（/requests/$requestId），列表页需要后端支持不带项目的查询，
    // 见 CHANGES_CN.md 的“后续事项”。在那之前不放进导航。
  },
  {
    path: '/system',
    titleKey: 'sidebar.items.system',
    icon: IconSettings,
    requiredScopes: ['read_settings'],
    scopeLevel: SYSTEM,
    nav: {
      SIMPLE: { section: 'site', order: 80, titleKey: 'nav.items.systemSettings' },
      ENTERPRISE: { section: 'system', order: 60, titleKey: 'nav.items.systemSettings' },
    },
  },
  {
    // 简单模式改价保存即生效，不走草稿审批，所以这两页只在企业模式出现
    path: '/change-sets',
    titleKey: 'sidebar.items.changeSets',
    icon: ClipboardCheck,
    modes: ['ENTERPRISE'],
    requiredScopes: GOVERNANCE_ROUTE_SCOPES,
    scopeLevel: SYSTEM,
    nav: { ENTERPRISE: { section: 'operations', order: 40 } },
  },
  {
    path: '/changelog',
    titleKey: 'sidebar.items.changelog',
    icon: History,
    modes: ['ENTERPRISE'],
    requiredScopes: GOVERNANCE_ROUTE_SCOPES,
    scopeLevel: SYSTEM,
    nav: { ENTERPRISE: { section: 'operations', order: 50 } },
  },
  {
    path: '/projects',
    titleKey: 'sidebar.items.projects',
    icon: IconPackages,
    modes: ['ENTERPRISE'],
    requiredScopes: ['read_projects'],
    scopeLevel: SYSTEM,
    nav: { ENTERPRISE: { section: 'system', order: 20 } },
  },
  {
    path: '/roles',
    titleKey: 'sidebar.items.roles',
    icon: IconShield,
    modes: ['ENTERPRISE'],
    requiredScopes: ['read_roles'],
    scopeLevel: SYSTEM,
    nav: { ENTERPRISE: { section: 'system', order: 30, titleKey: 'nav.items.systemRoles' } },
  },
  {
    path: '/prompt-protection-rules',
    titleKey: 'sidebar.items.promptProtectionRules',
    icon: IconShield,
    modes: ['ENTERPRISE'],
    requiredScopes: ['read_channels'],
    scopeLevel: SYSTEM,
    nav: { ENTERPRISE: { section: 'supply', order: 30 } },
  },
  {
    path: '/data-storages',
    titleKey: 'sidebar.items.dataStorages',
    icon: IconDatabase,
    modes: ['ENTERPRISE'],
    // 读权限可看、写权限可改（以前路由守卫要写权限、配置要读权限、行操作只看站长，三处不一致）
    requiredScopes: ['read_data_storages'],
    scopeLevel: SYSTEM,
    nav: { ENTERPRISE: { section: 'system', order: 40 } },
  },
  {
    path: '/api-keys',
    titleKey: 'sidebar.items.apiKeys',
    requiredScopes: ['read_api_keys'],
    scopeLevel: SYSTEM,
  },
];

/** 从注册表里找一个路径对应的路由：先完全相等，再按最长前缀（'/' 只做完全相等）。 */
export function findAppRoute(pathname: string): AppRoute | undefined {
  const path = pathname.length > 1 ? pathname.replace(/\/+$/, '') : pathname;
  let best: AppRoute | undefined;
  for (const route of APP_ROUTES) {
    const matches = route.path === path || (route.path !== '/' && path.startsWith(`${route.path}/`));
    if (matches && (!best || route.path.length > best.path.length)) {
      best = route;
    }
  }
  return best;
}

/** 某个模式下这个路由需要的权限（modeScopes 优先于 requiredScopes）。 */
export function requiredScopesFor(route: AppRoute, mode: ProductMode): string[] | undefined {
  return route.modeScopes?.[mode] ?? route.requiredScopes;
}
