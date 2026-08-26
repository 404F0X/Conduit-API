export function isNewApiChannelTag(tag: string): boolean {
  return (
    tag
      .trim()
      .toLowerCase()
      .replace(/[\s_-]+/g, '') === 'newapi'
  );
}
