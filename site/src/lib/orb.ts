// The app's orbs (crates/feeder-app/src/orb.rs) as CSS backgrounds: a pattern body under the same
// rim shade, bounce light, edge highlight and glints, so the site's orbs match the app's.

export type Pattern = 'nebula' | 'aurora' | 'pearl' | 'eclipse' | 'tide' | 'ember';

export const PATTERNS: Pattern[] = ['nebula', 'aurora', 'pearl', 'eclipse', 'tide', 'ember'];

export interface Orb {
  seed: number;
  hues: [number, number, number];
  chroma: number;
  angle: number;
  pattern: Pattern;
}

function noise(seed: number, i: number): number {
  let z = (seed ^ Math.imul(i + 1, 0x9e3779b1)) >>> 0;
  z = Math.imul(z ^ (z >>> 16), 0x85ebca6b) >>> 0;
  z = Math.imul(z ^ (z >>> 13), 0xc2b2ae35) >>> 0;
  return ((z ^ (z >>> 16)) >>> 0) / 4294967296;
}

function hue(o: Orb, i: number): number {
  const h0 = o.hues[0];
  if (i % 3 === 0) return h0;
  const hi = o.hues[i % 3]!;
  const delta = ((((hi - h0 + 540) % 360) + 360) % 360) - 180;
  return (((h0 + Math.max(-42, Math.min(42, delta))) % 360) + 360) % 360;
}

function tone(o: Orb, i: number, l: number, c: number, a = 1): string {
  const chroma = Math.min(Math.max(o.chroma, 0.15) * c, 0.24);
  return `oklch(${l.toFixed(3)} ${chroma.toFixed(3)} ${hue(o, i).toFixed(1)} / ${a})`;
}

const white = (a: number) => `rgb(255 255 255 / ${a})`;
const black = (a: number) => `rgb(0 0 0 / ${a})`;
const clear = 'transparent';

/** `radial-gradient` sized in fractions of the orb's radius, like the app's. */
function radial(cx: number, cy: number, rx: number, ry: number, stops: string): string {
  return `radial-gradient(${(rx * 50).toFixed(2)}% ${(ry * 50).toFixed(2)}% at ${(cx * 100).toFixed(1)}% ${(cy * 100).toFixed(1)}%, ${stops})`;
}

function base(o: Orb): string {
  switch (o.pattern) {
    case 'nebula': return tone(o, 0, 0.44, 0.95);
    case 'aurora': return tone(o, 0, 0.48, 0.95);
    case 'pearl': return tone(o, 0, 0.78, 0.55);
    case 'eclipse': return tone(o, 0, 0.28, 0.7);
    case 'tide': return tone(o, 1, 0.46, 0.95);
    case 'ember': return tone(o, 1, 0.36, 0.9);
  }
}

function pattern(o: Orb): string[] {
  const n = (i: number) => noise(o.seed, i);
  const deg = (o.angle * 180) / Math.PI;
  switch (o.pattern) {
    case 'nebula':
      return [0, 1, 2, 3].map(i => {
        const c = (a: number) => tone(o, i, 0.62 + n(i * 7) * 0.14, 1, a);
        const rad = 0.6 + n(i * 3 + 2) * 0.5;
        return radial(0.2 + n(i * 3) * 0.6, 0.2 + n(i * 3 + 1) * 0.6, rad, rad, `${c(0.85)}, ${c(0.28)} 60%, ${c(0)}`);
      });
    case 'aurora': {
      const haze = (a: number) => tone(o, 2, 0.74, 0.95, a);
      return [
        radial(0.32 + n(1) * 0.36, 0.32 + n(2) * 0.36, 0.75, 0.75, `${haze(0.45)}, ${haze(0)}`),
        `linear-gradient(${deg}deg, ${tone(o, 0, 0.46, 0.95)}, ${tone(o, 1, 0.64, 1)} 35%, ${tone(o, 2, 0.76, 0.9)} 55%, ${tone(o, 0, 0.58, 1)} 75%, ${tone(o, 1, 0.42, 0.95)})`,
      ];
    }
    case 'pearl':
      return [0, 1, 2, 3].map(i => {
        const c = (a: number) => tone(o, i, 0.82 + n(i) * 0.06, 0.72, a);
        const rad = 0.65 + n(i * 5 + 2) * 0.45;
        return radial(0.15 + n(i * 5) * 0.7, 0.15 + n(i * 5 + 1) * 0.7, rad, rad, `${c(0.85)}, ${c(0)}`);
      });
    case 'eclipse': {
      const core = tone(o, 2, 0.86, 0.65);
      const mid = (a: number) => tone(o, 2, 0.66, 1, a);
      const ring = (a: number) => tone(o, 1, 0.74, 1, a);
      const outer = (a: number) => tone(o, 0, 0.56, 1, a);
      return [
        radial(0.44 + n(1) * 0.12, 0.44 + n(2) * 0.12, 0.66, 0.66, `${core}, ${mid(0.85)} 40%, ${mid(0)}`),
        radial(0.5, 0.5, 1, 1, `${ring(0)} 56%, ${ring(0.8)} 72%, ${outer(0.42)} 86%, ${outer(0)}`),
      ];
    }
    case 'tide': {
      const h = 0.44 + n(4) * 0.12;
      const glow = (a: number) => tone(o, 2, 0.76, 0.95, a);
      const tilt = 180 + (deg - 180) * 0.25;
      return [
        radial(0.5, h, 1.1, 0.24, `${glow(0.5)}, ${glow(0)}`),
        `linear-gradient(${tilt}deg, ${tone(o, 0, 0.76, 0.9)}, ${tone(o, 0, 0.64, 1)} ${((h - 0.1) * 100).toFixed(0)}%, ${tone(o, 1, 0.5, 1)} ${((h + 0.1) * 100).toFixed(0)}%, ${tone(o, 1, 0.38, 0.95)})`,
      ];
    }
    case 'ember':
      return [
        radial(0.42 + n(1) * 0.16, 0.44 + n(2) * 0.16, 1.15, 1.15, `${tone(o, 2, 0.92, 0.55)}, ${tone(o, 0, 0.78, 1)} 28%, ${tone(o, 1, 0.56, 1)} 65%, ${tone(o, 1, 0.36, 0.9, 0)}`),
      ];
  }
}

/** The orb as a CSS `background` value; put it on a circle. */
export function orbBackground(o: Orb): string {
  const caustic = (a: number) => tone(o, 1, 0.86, 0.75, a);
  const lighting = [
    radial(0.72, 0.3, 0.06, 0.06, `${white(0.3)}, ${white(0)}`),
    radial(0.31, 0.21, 0.14, 0.1, `${white(0.88)}, ${white(0.28)} 60%, ${white(0)}`),
    radial(0.34, 0.25, 0.56, 0.4, `${white(0.42)}, ${white(0.14)} 38%, ${white(0)}`),
    radial(0.5, 0.5, 1, 1, `${clear} 91%, ${white(0.14)} 96.5%, ${clear}`),
    radial(0.62, 0.82, 0.58, 0.34, `${caustic(0.45)}, ${caustic(0.12)} 55%, ${caustic(0)}`),
    radial(0.5, 0.5, 1, 1, `${clear} 68%, ${black(0.12)} 90%, ${black(0.28)}`),
  ];
  return [...lighting, ...pattern(o), base(o)].join(', ');
}

/** The orb's signature colour, for glows. */
export function orbAccent(o: Orb, a = 1): string {
  return o.pattern === 'pearl' ? tone(o, 0, 0.8, 0.9, a) : tone(o, 0, 0.72, 1, a);
}

/** A fresh orb whose hue sits far from `avoid`, like the app's `Orb::generate`. */
export function randomOrb(avoid?: number): Orb {
  const r = Math.random;
  let h0 = r() * 360;
  if (avoid !== undefined) h0 = (avoid + 90 + r() * 180) % 360;
  const spread = 20 + r() * 30;
  return {
    seed: Math.floor(r() * 2 ** 31),
    hues: [h0, (h0 + spread) % 360, (h0 - spread + 360) % 360],
    chroma: 0.17 + r() * 0.06,
    angle: r() * Math.PI * 2,
    pattern: PATTERNS[Math.floor(r() * PATTERNS.length)]!,
  };
}

export const ORBS: Record<string, Orb> = {
  hero: { seed: 0x5eedf00d, hues: [292, 205, 350], chroma: 0.21, angle: 2.2, pattern: 'nebula' },
  orbit: { seed: 0xb0ba0002, hues: [168, 196, 140], chroma: 0.18, angle: 1.6, pattern: 'tide' },
  atlas: { seed: 0xa11ce001, hues: [32, 12, 58], chroma: 0.2, angle: 0.9, pattern: 'ember' },
  lumen: { seed: 0xd00d0004, hues: [310, 280, 340], chroma: 0.12, angle: 0.4, pattern: 'pearl' },
};
