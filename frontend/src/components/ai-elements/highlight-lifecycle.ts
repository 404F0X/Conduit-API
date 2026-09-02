export type HighlightedHtml = {
  light: string;
  dark: string;
};

export type HighlightRenderState = HighlightedHtml & {
  isLoading: boolean;
};

export function createHighlightState(preRenderedHtml?: HighlightedHtml): HighlightRenderState {
  if (preRenderedHtml) {
    return {
      ...preRenderedHtml,
      isLoading: false,
    };
  }

  return {
    light: '',
    dark: '',
    isLoading: true,
  };
}

export function settleHighlightWhileActive(
  highlight: Promise<readonly [string, string]>,
  onSettled: (state: HighlightRenderState) => void
): () => void {
  let active = true;

  void highlight.then(
    ([light, dark]) => {
      if (active) {
        onSettled({ light, dark, isLoading: false });
      }
    },
    () => {
      if (active) {
        onSettled({ light: '', dark: '', isLoading: false });
      }
    }
  );

  return () => {
    active = false;
  };
}
