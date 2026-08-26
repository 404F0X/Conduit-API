import { LanguageSwitch } from '@/components/language-switch';

interface Props {
  children: React.ReactNode;
}

export default function AuthLayout({ children }: Props) {
  return (
    <div className='tech relative min-h-screen overflow-hidden bg-[#1A1A1A]'>
      {/* Tech grid background */}
      <div aria-hidden='true' className='tech-grid pointer-events-none absolute inset-0 opacity-30'></div>

      {/* Low-poly network pattern */}
      <div aria-hidden='true' className='low-poly-network pointer-events-none absolute inset-0'></div>

      {/* Top Navigation (overlay) */}
      <nav className='absolute top-0 right-0 left-0 z-50 flex items-center justify-between p-6'>
        <div className='flex items-center space-x-3'>
          <img src='/logo.svg' alt='Conduit API logo' className='h-8 w-8 rounded-sm shadow-sm ring-1 ring-emerald-400/20' />
          <h1 className='bg-gradient-to-r from-emerald-300 to-teal-200 bg-clip-text text-2xl font-semibold text-transparent'>
            Conduit API
          </h1>
        </div>

        <div className='flex items-center space-x-2'>
          <LanguageSwitch />
        </div>
      </nav>

      {/* Main Content Area - children control layout; full height since header overlays */}
      <main className='relative z-10 min-h-screen'>{children}</main>
    </div>
  );
}
