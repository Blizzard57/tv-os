import { useEffect, useState } from 'react';

export interface ArtworkPalette {
  primary: string;
  secondary: string;
}

const cache = new Map<string, ArtworkPalette>();

function fallback(key: string): ArtworkPalette {
  let hash = 2166136261;
  for (let i = 0; i < key.length; i += 1) hash = Math.imul(hash ^ key.charCodeAt(i), 16777619);
  const hue = Math.abs(hash) % 360;
  return {
    primary: hslToRgb(hue, 34, 32),
    secondary: hslToRgb((hue + 64) % 360, 28, 24),
  };
}

function hslToRgb(h: number, s: number, l: number): string {
  const sat = s / 100;
  const light = l / 100;
  const c = (1 - Math.abs(2 * light - 1)) * sat;
  const x = c * (1 - Math.abs(((h / 60) % 2) - 1));
  const m = light - c / 2;
  const [r, g, b] = h < 60 ? [c, x, 0] : h < 120 ? [x, c, 0] : h < 180 ? [0, c, x]
    : h < 240 ? [0, x, c] : h < 300 ? [x, 0, c] : [c, 0, x];
  return [r, g, b].map((value) => Math.round((value + m) * 255)).join(' ');
}

function sample(image: HTMLImageElement): ArtworkPalette {
  const canvas = document.createElement('canvas');
  canvas.width = 24;
  canvas.height = 24;
  const context = canvas.getContext('2d', { willReadFrequently: true });
  if (!context) throw new Error('canvas unavailable');
  context.drawImage(image, 0, 0, 24, 24);
  const pixels = context.getImageData(0, 0, 24, 24).data;
  const buckets = [[0, 0, 0, 0], [0, 0, 0, 0]];
  for (let i = 0; i < pixels.length; i += 16) {
    const r = pixels[i], g = pixels[i + 1], b = pixels[i + 2], alpha = pixels[i + 3];
    const max = Math.max(r, g, b), min = Math.min(r, g, b);
    if (alpha < 220 || max < 30 || max - min < 18) continue;
    const bucket = r + g > b * 2 ? buckets[0] : buckets[1];
    bucket[0] += r; bucket[1] += g; bucket[2] += b; bucket[3] += 1;
  }
  const normalize = (bucket: number[], otherwise: string) => bucket[3]
    ? bucket.slice(0, 3).map((value) => Math.round((value / bucket[3]) * 0.58)).join(' ')
    : otherwise;
  return { primary: normalize(buckets[0], '58 72 96'), secondary: normalize(buckets[1], '46 62 82') };
}

/** Throttled, cancellable artwork sampling. Cross-origin failures fall back to
 * a stable muted hue, so rapid focus never flashes stale or over-bright color. */
export function useArtworkPalette(url: string | undefined, identity: string): ArtworkPalette {
  const key = url || identity;
  const [palette, setPalette] = useState(() => cache.get(key) || fallback(identity));
  useEffect(() => {
    let cancelled = false;
    const cached = cache.get(key);
    if (cached) {
      setPalette(cached);
      return;
    }
    setPalette(fallback(identity));
    if (!url) return;
    const timer = window.setTimeout(() => {
      const image = new Image();
      image.crossOrigin = 'anonymous';
      image.onload = () => {
        if (cancelled) return;
        let next: ArtworkPalette;
        try { next = sample(image); } catch { next = fallback(identity); }
        cache.set(key, next);
        if (!cancelled) setPalette(next);
      };
      image.onerror = () => {
        const next = fallback(identity);
        cache.set(key, next);
        if (!cancelled) setPalette(next);
      };
      image.src = url;
    }, 90);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [identity, key, url]);
  return palette;
}
