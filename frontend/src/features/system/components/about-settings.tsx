'use client';

import { ExternalLink } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { useSystemVersion } from '../data/system';

export function AboutSettings() {
  const { t } = useTranslation();
  const { data: version, isLoading: versionLoading } = useSystemVersion();

  if (versionLoading) {
    return (
      <div className='space-y-6'>
        <Card>
          <CardHeader>
            <Skeleton className='h-6 w-48' />
            <Skeleton className='h-4 w-72' />
          </CardHeader>
          <CardContent className='space-y-4'>
            {[1, 2, 3, 4, 5].map((i) => (
              <Skeleton key={i} className='h-4 w-full' />
            ))}
          </CardContent>
        </Card>
      </div>
    );
  }

  return (
    <div className='space-y-6'>
      <Card>
        <CardHeader>
          <CardTitle>{t('system.about.title')}</CardTitle>
          <CardDescription>{t('system.about.description')}</CardDescription>
        </CardHeader>
        <CardContent className='space-y-6'>
          {/* Version Info */}
          <div className='space-y-4'>
            <div className='flex items-center justify-between'>
              <span className='text-muted-foreground text-sm'>{t('system.about.version')}</span>
              <Badge variant='secondary' className='font-mono'>
                {version?.version || '-'}
              </Badge>
            </div>

            {version?.commit && (
              <div className='flex items-center justify-between'>
                <span className='text-muted-foreground text-sm'>{t('system.about.commit')}</span>
                <span className='font-mono text-sm'>{version.commit.substring(0, 7)}</span>
              </div>
            )}

            {version?.buildTime && (
              <div className='flex items-center justify-between'>
                <span className='text-muted-foreground text-sm'>{t('system.about.buildTime')}</span>
                <span className='text-sm'>{version.buildTime}</span>
              </div>
            )}

            <div className='flex items-center justify-between'>
              <span className='text-muted-foreground text-sm'>{t('system.about.rustVersion')}</span>
              <span className='text-sm'>{version?.rustVersion || '-'}</span>
            </div>

            <div className='flex items-center justify-between'>
              <span className='text-muted-foreground text-sm'>{t('system.about.platform')}</span>
              <span className='text-sm'>{version?.platform || '-'}</span>
            </div>

            <div className='flex items-center justify-between'>
              <span className='text-muted-foreground text-sm'>{t('system.about.uptime')}</span>
              <span className='text-sm'>{version?.uptime || '-'}</span>
            </div>
          </div>

          {/* Links */}
          <div className='border-t pt-6'>
            <h4 className='mb-4 text-sm font-medium'>{t('system.about.links.title')}</h4>
            <div className='flex flex-wrap gap-4'>
              <Button variant='outline' size='sm' asChild>
                <a href='https://github.com/404F0X/Conduit-API' target='_blank' rel='noopener noreferrer'>
                  GitHub
                  <ExternalLink className='ml-1 h-3 w-3' />
                </a>
              </Button>
              <Button variant='outline' size='sm' asChild>
                <a href='https://github.com/404F0X/Conduit-API/releases' target='_blank' rel='noopener noreferrer'>
                  {t('system.about.links.releases')}
                  <ExternalLink className='ml-1 h-3 w-3' />
                </a>
              </Button>
              <Button variant='outline' size='sm' asChild>
                <a href='https://github.com/404F0X/Conduit-API/issues' target='_blank' rel='noopener noreferrer'>
                  {t('system.about.links.issues')}
                  <ExternalLink className='ml-1 h-3 w-3' />
                </a>
              </Button>
            </div>
          </div>
        </CardContent>
      </Card>
    </div>
  );
}
