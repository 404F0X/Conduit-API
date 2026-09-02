import { lazy, Suspense, type ComponentType, type LazyExoticComponent } from 'react';

type ModelIconComponent = ComponentType<{ className?: string }>;
type ModelIconModule = { default: ModelIconComponent };

// Model icons are user-selected and the catalog contains hundreds of entries.
// Loading the package namespace pulls the entire icon catalog and its UI peer
// dependencies into one route chunk. Keep each icon behind its own import so a
// page only downloads the icons it actually renders.
const modelIconModules = import.meta.glob<ModelIconModule>('../../../../node_modules/@lobehub/icons/es/*/components/Mono.js');
const modelIconImporters = new Map(
  Object.entries(modelIconModules).flatMap(([modulePath, importer]) => {
    const iconName = modulePath.match(/\/es\/([^/]+)\/components\/Mono\.js$/)?.[1];
    return iconName ? ([[iconName, importer]] as const) : [];
  })
);
const lazyModelIcons = new Map<string, LazyExoticComponent<ModelIconComponent>>();

function getLazyModelIcon(iconName: string): LazyExoticComponent<ModelIconComponent> | undefined {
  const cached = lazyModelIcons.get(iconName);
  if (cached) return cached;

  const importer = modelIconImporters.get(iconName);
  if (!importer) return undefined;

  const icon = lazy(importer);
  lazyModelIcons.set(iconName, icon);
  return icon;
}

export function ModelIcon({ name }: { name?: string | null }) {
  const IconComponent = name ? getLazyModelIcon(name) : undefined;

  if (!IconComponent) return <span className='text-muted-foreground text-xs'>-</span>;

  return (
    <Suspense fallback={<span className='text-muted-foreground text-xs'>-</span>}>
      <IconComponent className='h-5 w-5' />
    </Suspense>
  );
}
