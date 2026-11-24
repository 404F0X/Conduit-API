import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { graphqlRequest } from '@/gql/graphql';

export type RetailFormulaInput = {
  priceBookID: string;
  publicModelID: string;
  deploymentID: string;
  expression: string;
  parameters: Record<string, string>;
  enabled: boolean;
  expectedRevision: string;
};
export type RetailFormula = Omit<RetailFormulaInput, 'expectedRevision'> & {
  id: string;
  revision: string;
  status: string;
  lastError?: string | null;
  lastFingerprint?: string | null;
  lastChangeSetID?: string | null;
};
export type FormulaPreview = {
  price: unknown;
  sourcePrice: unknown;
  currency: string;
  fingerprint: string;
};
const FIELDS =
  'id priceBookID publicModelID deploymentID expression parameters enabled revision status lastError lastFingerprint lastChangeSetID';
export function useRetailFormulas(bookID: string) {
  return useQuery({
    queryKey: ['retail-price-formulas', bookID],
    queryFn: () => getRetailFormulas(bookID),
    enabled: Boolean(bookID),
  });
}
export function getRetailFormulas(bookID: string) {
  return graphqlRequest<{ retailPriceFormulas: RetailFormula[] }>(
    `query RetailFormulas($priceBookID: ID!) { retailPriceFormulas(priceBookID: $priceBookID) { ${FIELDS} } }`,
    { priceBookID: bookID }
  ).then((data) => data.retailPriceFormulas);
}
export function previewRetailFormula(input: RetailFormulaInput) {
  return graphqlRequest<{ previewRetailPriceFormula: FormulaPreview }>(
    `query PreviewRetailFormula($input: RetailPriceFormulaInput!) { previewRetailPriceFormula(input: $input) { price sourcePrice currency fingerprint } }`,
    { input }
  ).then((data) => data.previewRetailPriceFormula);
}
export function useSaveRetailFormula() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: saveRetailFormula,
    onSuccess: () =>
      Promise.all([
        client.invalidateQueries({ queryKey: ['retail-price-formulas'] }),
        client.invalidateQueries({ queryKey: ['change-sets'] }),
      ]),
  });
}
export function saveRetailFormula(input: RetailFormulaInput) {
  return graphqlRequest<{ saveRetailPriceFormula: RetailFormula }>(
    `mutation SaveRetailFormula($input: RetailPriceFormulaInput!) { saveRetailPriceFormula(input: $input) { ${FIELDS} } }`,
    { input }
  ).then((data) => data.saveRetailPriceFormula);
}
