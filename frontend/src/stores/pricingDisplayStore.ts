import { create } from 'zustand';

export type PricingDisplayMode = 'accounting' | 'credits';

const STORAGE_KEY = 'conduit_admin_pricing_display_mode';

function initialMode(): PricingDisplayMode {
  try {
    return localStorage.getItem(STORAGE_KEY) === 'credits' ? 'credits' : 'accounting';
  } catch {
    return 'accounting';
  }
}

type PricingDisplayState = {
  mode: PricingDisplayMode;
  setMode: (mode: PricingDisplayMode) => void;
};

export const usePricingDisplayStore = create<PricingDisplayState>()((set) => ({
  mode: initialMode(),
  setMode: (mode) => {
    set({ mode });
    try {
      localStorage.setItem(STORAGE_KEY, mode);
    } catch {
      // Display preference can remain session-local when storage is unavailable.
    }
  },
}));
