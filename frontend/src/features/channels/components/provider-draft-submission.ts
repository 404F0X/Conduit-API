// The dialog preserves its pending intent across close, but only the current
// session can transition it. Its in-flight owner releases the lock exactly once.
export function createProviderDraftSubmission() {
  let generation = 0;
  let owner: symbol | null = null;
  return {
    invalidate() {
      generation++;
    },
    start() {
      if (owner) return null;
      const id = Symbol();
      const session = generation;
      owner = id;
      return {
        isCurrent: () => owner === id && generation === session,
        finish() {
          if (owner !== id) return false;
          owner = null;
          return true;
        },
      };
    },
  };
}
