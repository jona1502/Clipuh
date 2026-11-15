/**
 * Deterministic inline-SVG clip thumbnails. Colors come from a small palette
 * constant exposed as CSS custom properties on the root <svg> (`--t-*`);
 * shapes only reference those vars, never raw hex elsewhere.
 */
import { useId, type CSSProperties } from "react";

export type ThumbScene =
  "canyon" | "neon" | "forest" | "fortress" | "arena" | "desert";

interface Palette {
  skyTop: string;
  skyBot: string;
  far: string;
  mid: string;
  near: string;
  light: string;
}

const PALETTES: Record<ThumbScene, Palette> = {
  canyon: {
    skyTop: "#3a2a5c",
    skyBot: "#e08a5a",
    far: "#8a4a4a",
    mid: "#5c3040",
    near: "#2a1a2a",
    light: "#ffd9a0",
  },
  neon: {
    skyTop: "#0c0a24",
    skyBot: "#4a1a6a",
    far: "#1c1a4a",
    mid: "#12103a",
    near: "#08061c",
    light: "#ff4fd8",
  },
  forest: {
    skyTop: "#2a3a7a",
    skyBot: "#f0a070",
    far: "#3a6a5a",
    mid: "#245040",
    near: "#102a22",
    light: "#fff0c0",
  },
  fortress: {
    skyTop: "#5a7aa0",
    skyBot: "#d8c8a0",
    far: "#8a8070",
    mid: "#5a5448",
    near: "#2a2620",
    light: "#fff4d0",
  },
  arena: {
    skyTop: "#0a1a2a",
    skyBot: "#1a5a6a",
    far: "#16404e",
    mid: "#0e2a36",
    near: "#06141c",
    light: "#60f0ff",
  },
  desert: {
    skyTop: "#d88a5a",
    skyBot: "#f8d890",
    far: "#c88a50",
    mid: "#a86a38",
    near: "#5a3418",
    light: "#fffae0",
  },
};

/** Small seeded PRNG (mulberry32): same seed, same picture. */
function rng(seed: number) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const W = 320;
const H = 180;

function ridge(
  r: () => number,
  base: number,
  amp: number,
  steps: number,
): string {
  let d = `M0 ${H} L0 ${base}`;
  for (let i = 1; i <= steps; i++) {
    d += ` L${((i / steps) * W).toFixed(1)} ${(base - r() * amp).toFixed(1)}`;
  }
  return `${d} L${W} ${H} Z`;
}

function Shapes({ scene, r }: { scene: ThumbScene; r: () => number }) {
  const lightX = 40 + r() * 240;
  const lightY = 30 + r() * 30;
  const light = (
    <circle
      cx={lightX}
      cy={lightY}
      r={scene === "neon" ? 14 : 20}
      fill="var(--t-light)"
      opacity={0.85}
    />
  );
  switch (scene) {
    case "canyon":
    case "desert":
      return (
        <>
          {light}
          <path d={ridge(r, 110, 50, 9)} fill="var(--t-far)" />
          <path d={ridge(r, 135, 40, 7)} fill="var(--t-mid)" />
          <path d={ridge(r, 160, 25, 6)} fill="var(--t-near)" />
        </>
      );
    case "forest": {
      const trees = Array.from({ length: 14 }, (_, i) => {
        const x = (i / 14) * W + r() * 14;
        const h = 30 + r() * 40;
        return (
          <path
            key={i}
            d={`M${x} 150 L${x + 12} 150 L${x + 6} ${150 - h} Z`}
            fill="var(--t-near)"
          />
        );
      });
      return (
        <>
          {light}
          <path d={ridge(r, 105, 30, 8)} fill="var(--t-far)" />
          <path d={ridge(r, 130, 24, 8)} fill="var(--t-mid)" />
          {trees}
          <rect x="0" y="148" width={W} height="32" fill="var(--t-near)" />
        </>
      );
    }
    case "fortress": {
      const crenels = Array.from({ length: 10 }, (_, i) => (
        <rect
          key={i}
          x={30 + i * 26}
          y={72}
          width="14"
          height="14"
          fill="var(--t-mid)"
        />
      ));
      return (
        <>
          {light}
          <path d={ridge(r, 110, 30, 7)} fill="var(--t-far)" />
          <rect x="24" y="84" width="272" height="66" fill="var(--t-mid)" />
          {crenels}
          <rect
            x="140"
            y="100"
            width="40"
            height="50"
            rx="20"
            fill="var(--t-near)"
          />
          <rect x="0" y="148" width={W} height="32" fill="var(--t-near)" />
        </>
      );
    }
    case "neon": {
      const towers = Array.from({ length: 12 }, (_, i) => {
        const h = 40 + r() * 80;
        const w = 18 + r() * 12;
        return (
          <rect
            key={i}
            x={i * 27}
            y={150 - h}
            width={w}
            height={h}
            fill={i % 2 ? "var(--t-mid)" : "var(--t-far)"}
          />
        );
      });
      return (
        <>
          {light}
          {towers}
          <rect x="0" y="146" width={W} height="34" fill="var(--t-near)" />
          <rect
            x="0"
            y="146"
            width={W}
            height="2"
            fill="var(--t-light)"
            opacity={0.7}
          />
        </>
      );
    }
    case "arena": {
      const pillars = Array.from({ length: 6 }, (_, i) => (
        <rect
          key={i}
          x={20 + i * 52}
          y={50 + r() * 20}
          width="10"
          height="100"
          fill="var(--t-mid)"
        />
      ));
      return (
        <>
          {light}
          <ellipse cx="160" cy="150" rx="200" ry="40" fill="var(--t-far)" />
          {pillars}
          <ellipse cx="160" cy="160" rx="120" ry="18" fill="var(--t-near)" />
          <ellipse
            cx="160"
            cy="160"
            rx="60"
            ry="8"
            fill="var(--t-light)"
            opacity={0.35}
          />
        </>
      );
    }
  }
}

export function ClipThumb({
  scene,
  seed,
  className,
}: {
  scene: ThumbScene;
  seed: number;
  className?: string;
}) {
  const gid = useId();
  const p = PALETTES[scene];
  const style = {
    "--t-sky-top": p.skyTop,
    "--t-sky-bot": p.skyBot,
    "--t-far": p.far,
    "--t-mid": p.mid,
    "--t-near": p.near,
    "--t-light": p.light,
  } as CSSProperties;
  return (
    <svg
      viewBox={`0 0 ${W} ${H}`}
      preserveAspectRatio="xMidYMid slice"
      className={className}
      style={style}
      role="img"
      aria-label={`${scene} clip preview`}
    >
      <defs>
        <linearGradient id={gid} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="var(--t-sky-top)" />
          <stop offset="1" stopColor="var(--t-sky-bot)" />
        </linearGradient>
      </defs>
      <rect width={W} height={H} fill={`url(#${gid})`} />
      <Shapes scene={scene} r={rng(seed * 9973 + scene.length)} />
    </svg>
  );
}
