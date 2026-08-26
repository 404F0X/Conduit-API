import { type FC, useCallback, useEffect, useRef } from 'react';
import type { FormBounds, MouseArea, Particle } from './animated-line-background.engine';
import {
  advanceFrameTiming,
  animationConfig,
  getFormBounds,
  getParticleCount,
  initParticles,
  renderParticles,
  shouldAnimateCanvas,
  updateParticles,
} from './animated-line-background.engine';

interface AnimationDiagnosticsSnapshot {
  targetFps: number;
  frameIntervalMs: number;
  maxCatchUpMs: number;
  maxStepsPerFrame: number;
  frameCount: number;
  simulationStepCount: number;
  renderCount: number;
  simulatedMs: number;
  accumulatorMs: number;
  lastFrameDeltaMs: number;
  lastClampedDeltaMs: number;
  lastFrameStepCount: number;
  lastAppliedDeltaMs: number;
  particleChecksum: number;
}

interface AnimationDiagnostics {
  reset(): void;
  snapshot(): AnimationDiagnosticsSnapshot;
  simulate(stepMs: number, steps: number): void;
  simulateLargeGap(deltaMs: number): void;
}

declare global {
  interface Window {
    __CONDUIT_SIGNIN_ANIMATION__?: AnimationDiagnostics;
  }
}

const DEBUG_QUERY_PARAM = '__conduit_debug_animation';

const createMouseArea = (): MouseArea => ({ x: null, y: null, max: 20000 });

const cloneParticles = (particles: Particle[]): Particle[] => particles.map((particle) => ({ ...particle }));

const getParticleChecksum = (particles: Particle[]): number => {
  return particles.reduce((checksum, particle, index) => {
    const x = Math.round(particle.x * 1000);
    const y = Math.round(particle.y * 1000);
    const factor = index + 1;

    return checksum + x * factor * 31 + y * factor * 17;
  }, 0);
};

const shouldExposeAnimationDiagnostics = (): boolean => {
  if (!import.meta.env.DEV || typeof window === 'undefined') {
    return false;
  }

  return new URLSearchParams(window.location.search).get(DEBUG_QUERY_PARAM) === '1';
};

const AnimatedLineBackground: FC = () => {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const ctxRef = useRef<CanvasRenderingContext2D | null>(null);
  const animationRef = useRef<number | null>(null);
  const animationEnabledRef = useRef(false);
  const particlesRef = useRef<Particle[]>([]);
  const diagnosticsInitialParticlesRef = useRef<Particle[] | null>(null);
  const diagnosticsCanvasSizeRef = useRef<{ width: number; height: number } | null>(null);
  const formBoundsRef = useRef<FormBounds | null>(null);
  const mouseAreaRef = useRef<MouseArea>(createMouseArea());
  const frameCountRef = useRef(0);
  const simulationStepCountRef = useRef(0);
  const renderCountRef = useRef(0);
  const simulatedMsRef = useRef(0);
  const accumulatorRef = useRef(0);
  const lastTimestampRef = useRef<number | null>(null);
  const lastFrameDeltaMsRef = useRef(0);
  const lastClampedDeltaMsRef = useRef(0);
  const lastFrameStepCountRef = useRef(0);
  const lastAppliedDeltaMsRef = useRef(0);

  const getMeasuredFormBounds = useCallback((): FormBounds | null => {
    const el = document.getElementById('auth-card-wrapper');
    if (!el) return null;
    const rect = el.getBoundingClientRect();
    return {
      formCenterX: rect.left + rect.width / 2,
      formCenterY: rect.top + rect.height / 2,
      formLeft: rect.left,
      formRight: rect.right,
      formTop: rect.top,
      formBottom: rect.bottom,
    };
  }, []);

  const updateFormBounds = useCallback(
    (canvasWidth: number, canvasHeight: number) => {
      const newBounds = getMeasuredFormBounds() ?? getFormBounds(canvasWidth, canvasHeight);
      const prev = formBoundsRef.current;

      if (
        !prev ||
        prev.formLeft !== newBounds.formLeft ||
        prev.formRight !== newBounds.formRight ||
        prev.formTop !== newBounds.formTop ||
        prev.formBottom !== newBounds.formBottom
      ) {
        formBoundsRef.current = newBounds;
        return true;
      }
      return false;
    },
    [getMeasuredFormBounds]
  );

  const resize = useCallback(() => {
    const canvas = canvasRef.current;
    if (!canvas) return { sizeChanged: false, boundsChanged: false };

    const prevWidth = canvas.width;
    const prevHeight = canvas.height;

    const nextWidth = window.innerWidth;
    const nextHeight = window.innerHeight;
    if (canvas.width !== nextWidth) canvas.width = nextWidth;
    if (canvas.height !== nextHeight) canvas.height = nextHeight;

    ctxRef.current = ctxRef.current ?? canvas.getContext('2d');
    const boundsChanged = updateFormBounds(canvas.width, canvas.height);
    const sizeChanged = prevWidth !== canvas.width || prevHeight !== canvas.height;

    return { sizeChanged, boundsChanged };
  }, [updateFormBounds]);

  const handleResize = useCallback(() => {
    const canvas = canvasRef.current;
    if (!canvas || document.visibilityState !== 'visible') return;

    const { sizeChanged, boundsChanged } = resize();

    if (!animationEnabledRef.current) {
      particlesRef.current = [];
      ctxRef.current?.clearRect(0, 0, canvas.width, canvas.height);
      return;
    }

    const ctx = ctxRef.current;
    if (!ctx) return;

    const particleCount = getParticleCount(canvas.width);
    if (sizeChanged || boundsChanged || particlesRef.current.length !== particleCount) {
      const formBounds = formBoundsRef.current ?? getFormBounds(canvas.width, canvas.height);
      particlesRef.current = initParticles(canvas.width, canvas.height, formBounds, particleCount);
    }

    renderParticles(
      ctx,
      canvas.width,
      canvas.height,
      particlesRef.current,
      mouseAreaRef.current,
      formBoundsRef.current ?? getFormBounds(canvas.width, canvas.height)
    );
  }, [resize]);

  const resetFrameTimingState = useCallback(() => {
    accumulatorRef.current = 0;
    lastTimestampRef.current = null;
    lastFrameDeltaMsRef.current = 0;
    lastClampedDeltaMsRef.current = 0;
    lastFrameStepCountRef.current = 0;
    lastAppliedDeltaMsRef.current = 0;
  }, []);

  const renderFrame = useCallback(() => {
    const canvas = canvasRef.current;
    const ctx = ctxRef.current;
    const formBounds = formBoundsRef.current;
    if (!canvas || !ctx || !formBounds) return;

    renderParticles(ctx, canvas.width, canvas.height, particlesRef.current, mouseAreaRef.current, formBounds);

    renderCountRef.current += 1;
  }, []);

  const applyAnimationStep = useCallback((deltaMs: number) => {
    const canvas = canvasRef.current;
    const formBounds = formBoundsRef.current;
    if (!canvas || !formBounds) return;

    updateParticles(canvas.width, canvas.height, particlesRef.current, mouseAreaRef.current, formBounds);

    simulationStepCountRef.current += 1;
    simulatedMsRef.current += deltaMs;
    lastAppliedDeltaMsRef.current = deltaMs;
  }, []);

  const processAnimationFrame = useCallback(
    (deltaMs: number) => {
      const timing = advanceFrameTiming(accumulatorRef.current, deltaMs);

      lastFrameDeltaMsRef.current = timing.frameDeltaMs;
      lastClampedDeltaMsRef.current = timing.clampedDeltaMs;
      accumulatorRef.current = timing.accumulatorMs;

      const steps = timing.shouldStep ? 1 : 0;
      if (timing.shouldStep) {
        applyAnimationStep(animationConfig.frameIntervalMs);
      }

      lastFrameStepCountRef.current = steps;
      if (steps === 0) {
        lastAppliedDeltaMsRef.current = 0;
      }

      if (steps > 0) {
        renderFrame();
      }
    },
    [applyAnimationStep, renderFrame]
  );

  const resetDiagnosticsState = useCallback(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    frameCountRef.current = 0;
    simulationStepCountRef.current = 0;
    renderCountRef.current = 0;
    simulatedMsRef.current = 0;
    resetFrameTimingState();
    mouseAreaRef.current = createMouseArea();

    const { sizeChanged, boundsChanged } = resize();

    const nextCanvasSize = { width: canvas.width, height: canvas.height };
    const needsNewInitialParticles =
      sizeChanged ||
      boundsChanged ||
      diagnosticsInitialParticlesRef.current === null ||
      diagnosticsCanvasSizeRef.current?.width !== nextCanvasSize.width ||
      diagnosticsCanvasSizeRef.current?.height !== nextCanvasSize.height;

    if (needsNewInitialParticles) {
      const formBounds = formBoundsRef.current ?? getFormBounds(nextCanvasSize.width, nextCanvasSize.height);
      diagnosticsInitialParticlesRef.current = initParticles(nextCanvasSize.width, nextCanvasSize.height, formBounds);
      diagnosticsCanvasSizeRef.current = nextCanvasSize;
    }

    const initialParticles = diagnosticsInitialParticlesRef.current;
    if (!initialParticles) return;

    particlesRef.current = cloneParticles(initialParticles);
    renderFrame();
    renderCountRef.current = 0;
  }, [renderFrame, resize, resetFrameTimingState]);

  const snapshotDiagnostics = useCallback<() => AnimationDiagnosticsSnapshot>(() => {
    return {
      targetFps: animationConfig.targetFps,
      frameIntervalMs: animationConfig.frameIntervalMs,
      maxCatchUpMs: animationConfig.maxCatchUpMs,
      maxStepsPerFrame: animationConfig.maxStepsPerFrame,
      frameCount: frameCountRef.current,
      simulationStepCount: simulationStepCountRef.current,
      renderCount: renderCountRef.current,
      simulatedMs: simulatedMsRef.current,
      accumulatorMs: accumulatorRef.current,
      lastFrameDeltaMs: lastFrameDeltaMsRef.current,
      lastClampedDeltaMs: lastClampedDeltaMsRef.current,
      lastFrameStepCount: lastFrameStepCountRef.current,
      lastAppliedDeltaMs: lastAppliedDeltaMsRef.current,
      particleChecksum: getParticleChecksum(particlesRef.current),
    };
  }, []);

  const simulateDiagnostics = useCallback(
    (stepMs: number, steps: number) => {
      const safeStepMs = Number.isFinite(stepMs) ? stepMs : 0;
      const safeSteps = Number.isFinite(steps) ? Math.max(0, Math.floor(steps)) : 0;

      for (let index = 0; index < safeSteps; index += 1) {
        frameCountRef.current += 1;
        processAnimationFrame(safeStepMs);
      }
    },
    [processAnimationFrame]
  );

  const simulateLargeGapDiagnostics = useCallback(
    (deltaMs: number) => {
      const safeDeltaMs = Number.isFinite(deltaMs) ? Math.max(0, deltaMs) : 0;

      frameCountRef.current += 1;
      processAnimationFrame(safeDeltaMs);
    },
    [processAnimationFrame]
  );

  const animate = useCallback(
    (timestamp: number) => {
      if (!animationEnabledRef.current || document.visibilityState !== 'visible') {
        animationRef.current = null;
        return;
      }

      frameCountRef.current += 1;

      if (lastTimestampRef.current === null) {
        lastTimestampRef.current = timestamp;
      }

      processAnimationFrame(timestamp - lastTimestampRef.current);
      lastTimestampRef.current = timestamp;

      animationRef.current = requestAnimationFrame(animate);
    },
    [processAnimationFrame]
  );

  const stopAnimation = useCallback(() => {
    if (animationRef.current === null) {
      return;
    }

    cancelAnimationFrame(animationRef.current);
    animationRef.current = null;
  }, []);

  const startAnimation = useCallback(() => {
    if (!animationEnabledRef.current || animationRef.current !== null || document.visibilityState !== 'visible') {
      return;
    }

    animationRef.current = requestAnimationFrame(animate);
  }, [animate]);

  const handleMouseMove = useCallback((e: MouseEvent) => {
    if (!animationEnabledRef.current) return;
    mouseAreaRef.current.x = e.clientX;
    mouseAreaRef.current.y = e.clientY;
  }, []);

  const handleMouseOut = useCallback(() => {
    if (!animationEnabledRef.current) return;
    mouseAreaRef.current.x = null;
    mouseAreaRef.current.y = null;
  }, []);

  useEffect(() => {
    const desktopViewport = window.matchMedia(`(min-width: ${animationConfig.minAnimatedViewportWidth}px)`);
    const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)');

    const syncAnimationState = () => {
      animationEnabledRef.current = shouldAnimateCanvas(window.innerWidth, reducedMotion.matches, document.visibilityState);
      resetFrameTimingState();

      if (animationEnabledRef.current) {
        handleResize();
        startAnimation();
      } else {
        stopAnimation();
        handleResize();
      }
    };

    syncAnimationState();

    window.addEventListener('resize', handleResize);
    window.addEventListener('mousemove', handleMouseMove);
    window.addEventListener('mouseout', handleMouseOut);
    desktopViewport.addEventListener('change', syncAnimationState);
    reducedMotion.addEventListener('change', syncAnimationState);
    document.addEventListener('visibilitychange', syncAnimationState);

    const el = document.getElementById('auth-card-wrapper');
    let observer: ResizeObserver | null = null;
    if (el) {
      observer = new ResizeObserver(() => {
        handleResize();
      });
      observer.observe(el);
    }

    return () => {
      window.removeEventListener('resize', handleResize);
      window.removeEventListener('mousemove', handleMouseMove);
      window.removeEventListener('mouseout', handleMouseOut);
      desktopViewport.removeEventListener('change', syncAnimationState);
      reducedMotion.removeEventListener('change', syncAnimationState);
      document.removeEventListener('visibilitychange', syncAnimationState);
      if (observer) {
        observer.disconnect();
      }
      stopAnimation();
    };
  }, [handleResize, handleMouseMove, handleMouseOut, resetFrameTimingState, startAnimation, stopAnimation]);

  useEffect(() => {
    if (!shouldExposeAnimationDiagnostics()) {
      delete window.__CONDUIT_SIGNIN_ANIMATION__;
      return;
    }

    window.__CONDUIT_SIGNIN_ANIMATION__ = {
      reset: resetDiagnosticsState,
      snapshot: snapshotDiagnostics,
      simulate: simulateDiagnostics,
      simulateLargeGap: simulateLargeGapDiagnostics,
    };

    return () => {
      delete window.__CONDUIT_SIGNIN_ANIMATION__;
    };
  }, [resetDiagnosticsState, simulateDiagnostics, simulateLargeGapDiagnostics, snapshotDiagnostics]);

  return (
    <canvas
      ref={canvasRef}
      aria-hidden='true'
      data-testid='sign-in-animation-canvas'
      className='pointer-events-none fixed inset-0 hidden motion-reduce:hidden lg:block'
      style={{ zIndex: 0 }}
    />
  );
};

export default AnimatedLineBackground;
