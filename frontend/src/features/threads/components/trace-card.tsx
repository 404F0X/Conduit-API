import { format } from 'date-fns';
import { zhCN, enUS } from 'date-fns/locale';
import { ArrowUpRight, User, Bot, Clock } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import type { Trace } from '@/features/traces/data/schema';

interface TraceCardProps {
  trace: Trace;
  onViewTrace: (traceId: string) => void;
  index?: number;
}

export function TraceCard({ trace, onViewTrace, index }: TraceCardProps) {
  const { t, i18n } = useTranslation();
  const locale = i18n.language === 'zh' ? zhCN : enUS;
  const createdAtLabel = format(trace.createdAt, 'yyyy-MM-dd HH:mm:ss', { locale });

  return (
    <Card className='group border-border/50 from-card to-card/95 hover:border-border relative overflow-hidden border bg-gradient-to-br shadow-sm transition-all duration-300 hover:-translate-y-0.5 hover:shadow-lg'>
      {/* Top accent line */}
      <div className='from-primary/60 via-primary to-primary/60 absolute top-0 right-0 left-0 h-0.5 bg-gradient-to-r opacity-0 transition-opacity duration-300 group-hover:opacity-100' />

      <CardContent className='p-5'>
        <div className='space-y-4'>
          {/* Header: Index + Trace ID + Created At */}
          <div className='flex items-center justify-between'>
            <div className='flex items-center gap-2.5'>
              {index !== undefined && (
                <Badge variant='secondary' className='h-6 min-w-6 justify-center rounded-md px-2 font-mono text-xs font-medium'>
                  #{index + 1}
                </Badge>
              )}
              <div className='bg-muted/50 flex items-center gap-1.5 rounded-md px-2 py-1'>
                <span className='text-muted-foreground font-mono text-xs'>{trace.traceID}</span>
              </div>
            </div>
            <div className='text-muted-foreground/80 flex items-center gap-1.5 text-xs'>
              <Clock className='h-3 w-3' />
              <span>{createdAtLabel}</span>
            </div>
          </div>

          {/* Chat Messages */}
          <div className='space-y-4 pt-1'>
            {/* User Query */}
            {trace.firstUserQuery && (
              <div className='flex items-start justify-end gap-2.5'>
                <div className='flex max-w-[85%] flex-col items-end gap-1'>
                  <div className='from-primary to-primary/90 text-primary-foreground relative rounded-2xl rounded-tr-sm bg-gradient-to-br px-4 py-2.5 shadow-sm'>
                    <p className='text-sm leading-relaxed'>{trace.firstUserQuery}</p>
                  </div>
                </div>
                <div className='bg-muted ring-background flex h-7 w-7 shrink-0 items-center justify-center rounded-full ring-2'>
                  <User className='text-muted-foreground h-3.5 w-3.5' />
                </div>
              </div>
            )}

            {/* Assistant Response */}
            {trace.firstText && (
              <div className='flex items-start gap-2.5'>
                <div className='bg-primary/10 ring-background flex h-7 w-7 shrink-0 items-center justify-center rounded-full ring-2'>
                  <Bot className='text-primary h-3.5 w-3.5' />
                </div>
                <div className='flex max-w-[85%] flex-col gap-1'>
                  <div className='bg-muted text-foreground relative rounded-2xl rounded-tl-sm px-4 py-2.5 shadow-sm'>
                    <p className='text-sm leading-relaxed whitespace-pre-wrap'>{trace.firstText}</p>
                  </div>
                </div>
              </div>
            )}
          </div>

          {/* Footer: Actions */}
          <div className='flex items-center justify-end pt-3'>
            <Button
              variant='ghost'
              size='sm'
              onClick={() => onViewTrace(trace.id)}
              className='group/button text-muted-foreground/80 hover:bg-primary/5 hover:text-primary h-8 gap-1.5 rounded-lg text-xs font-medium transition-colors'
            >
              {t('threads.detail.viewTrace')}
              <ArrowUpRight className='h-3.5 w-3.5 transition-transform group-hover/button:translate-x-0.5 group-hover/button:-translate-y-0.5' />
            </Button>
          </div>
        </div>
      </CardContent>
    </Card>
  );
}
