export type SystemRoleTemplate = {
  id: 'commercial_viewer' | 'subscription_operator' | 'credit_operator' | 'commercial_admin';
  nameKey: string;
  descriptionKey: string;
  highRisk?: boolean;
  scopes: readonly string[];
};

/**
 * Suggested starting points for new system roles. These are deliberately
 * client-side suggestions: applying one only fills the create form and never
 * migrates or expands an existing custom role.
 */
export const systemRoleTemplates: readonly SystemRoleTemplate[] = [
  {
    id: 'commercial_viewer',
    nameKey: 'roles.templates.commercialViewer.name',
    descriptionKey: 'roles.templates.commercialViewer.description',
    scopes: ['read_groups', 'read_subscriptions', 'read_billing', 'read_commercialization'],
  },
  {
    id: 'subscription_operator',
    nameKey: 'roles.templates.subscriptionOperator.name',
    descriptionKey: 'roles.templates.subscriptionOperator.description',
    scopes: ['read_users', 'read_groups', 'read_subscriptions', 'write_subscriptions', 'read_billing'],
  },
  {
    id: 'credit_operator',
    nameKey: 'roles.templates.creditOperator.name',
    descriptionKey: 'roles.templates.creditOperator.description',
    highRisk: true,
    scopes: ['read_users', 'read_billing', 'grant_credit'],
  },
  {
    id: 'commercial_admin',
    nameKey: 'roles.templates.commercialAdmin.name',
    descriptionKey: 'roles.templates.commercialAdmin.description',
    highRisk: true,
    scopes: [
      'read_users',
      'read_channels',
      'read_groups',
      'write_groups',
      'read_subscriptions',
      'write_subscriptions',
      'read_billing',
      'write_billing',
      'grant_credit',
      'read_commercialization',
      'write_commercialization',
    ],
  },
];
