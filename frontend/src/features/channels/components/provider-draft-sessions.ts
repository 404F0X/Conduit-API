import { GRAPHQL_ENDPOINT } from '@/gql/graphql';
import { createStore } from 'zustand/vanilla';
import { useAuthStore } from '@/stores/authStore';
import {
  createProviderDraftFromDialog,
  createProviderPackage,
  lookupProviderDraftReceipt,
  type CreateProviderResult,
} from '@/features/change-sets/data/change-sets';

type ProviderDraftIntent = Parameters<typeof createProviderPackage>[0];
export type ProviderDraftSlot = { actorID: string; endpoint: string; channelID: string };
type Entry = {
  intent: ProviderDraftIntent;
  status: 'busy' | 'unknown' | 'complete';
  result?: { outcome: CreateProviderResult['outcome']; packageID?: string };
};
type Recovery = 'check' | 'retry' | 'abandon';

export function providerDraftSlotKey(slot: ProviderDraftSlot) {
  return JSON.stringify([slot.actorID, slot.endpoint, slot.channelID]);
}

export function createProviderDraftSessions(
  transport: { create: typeof createProviderPackage; lookup: typeof lookupProviderDraftReceipt },
  isActorCurrent: (id: string) => boolean,
  newOperationID = () => crypto.randomUUID()
) {
  const store = createStore<{ entries: Record<string, Entry> }>()(() => ({ entries: {} }));

  function write(key: string, entry?: Entry) {
    const entries = { ...store.getState().entries };
    if (entry) entries[key] = entry;
    else delete entries[key];
    store.setState({ entries });
  }

  function stillOwned(slot: ProviderDraftSlot, key: string, entry: Entry) {
    return isActorCurrent(slot.actorID) && store.getState().entries[key] === entry;
  }

  async function run(slot: ProviderDraftSlot, entry: Entry, action: 'create' | Recovery) {
    const key = providerDraftSlotKey(slot);
    const busy: Entry = { ...entry, status: 'busy' };
    write(key, busy);
    const lookup: typeof lookupProviderDraftReceipt = (operationID) => {
      if (!stillOwned(slot, key, busy)) throw new Error('Provider draft session has ended');
      return transport.lookup(operationID);
    };
    try {
      let result = action === 'create' ? null : await lookup(entry.intent.operationID);
      if (!stillOwned(slot, key, busy)) return;
      if (result === null) {
        if (action === 'check' || action === 'abandon') {
          write(key, action === 'abandon' ? undefined : { ...entry, status: 'unknown' });
          return null;
        }
        const created = await createProviderDraftFromDialog(entry.intent, transport.create, lookup);
        if (!stillOwned(slot, key, busy)) return;
        if ('validationError' in created) {
          write(key);
          throw created.validationError;
        }
        result = created.result;
      }
      write(key, { ...entry, status: 'complete', result: { outcome: result.outcome, packageID: result.changeSetDetail?.summary.id } });
      return result;
    } catch (error) {
      if (stillOwned(slot, key, busy)) write(key, { ...entry, status: 'unknown' });
      throw error;
    }
  }

  return {
    store,
    clear: () => store.setState({ entries: {} }),
    start(slot: ProviderDraftSlot, input: Pick<ProviderDraftIntent, 'input' | 'expectedBaseRevision'>) {
      if (!isActorCurrent(slot.actorID) || store.getState().entries[providerDraftSlotKey(slot)]) return;
      const intent = { ...structuredClone(input), channelID: slot.channelID, operationID: newOperationID() };
      return run(slot, { intent, status: 'busy' }, 'create');
    },
    recover(slot: ProviderDraftSlot, action: Recovery) {
      const entry = store.getState().entries[providerDraftSlotKey(slot)];
      if (!isActorCurrent(slot.actorID) || entry?.status !== 'unknown') return;
      return run(slot, entry, action);
    },
    dismiss(slot: ProviderDraftSlot) {
      const key = providerDraftSlotKey(slot);
      if (isActorCurrent(slot.actorID) && store.getState().entries[key]?.status === 'complete') write(key);
    },
  };
}

export function currentProviderDraftSlot(actorID: string, channelID: string): ProviderDraftSlot {
  return { actorID, channelID, endpoint: new URL(GRAPHQL_ENDPOINT, window.location.origin).href };
}

export const providerDraftSessions = createProviderDraftSessions(
  { create: createProviderPackage, lookup: lookupProviderDraftReceipt },
  (id) => useAuthStore.getState().auth.user?.id === id
);

useAuthStore.subscribe(({ auth }, previous) => {
  if (auth.user?.id !== previous.auth.user?.id || (previous.auth.accessToken && !auth.accessToken)) providerDraftSessions.clear();
});
