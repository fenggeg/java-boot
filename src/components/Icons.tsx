/**
 * JavaBoot Launcher — icon set
 * Clean SF-Symbol-style line glyphs with round caps/joins.
 * All icons inherit `currentColor`; 24×24 viewBox unless noted.
 * App icon: blue-gradient squircle + white play glyph + spring wave (Spring Boot).
 */
import type {ReactNode, SVGProps} from "react";
import {useId} from "react";

type IconProps = SVGProps<SVGSVGElement> & { size?: number };

function Svg({ size = 16, children, ...rest }: IconProps & { children: ReactNode }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.7}
      strokeLinecap="round"
      strokeLinejoin="round"
      style={{ display: "inline-block", flexShrink: 0 }}
      {...rest}
    >
      {children}
    </svg>
  );
}

/* ── Brand mark: blue-gradient squircle app icon ────────────
   Matches the packaged app icon (src-tauri/icons).
   White rounded play glyph = launcher; green spring wave = Spring Boot. */
export function Logo({ size = 24, ...rest }: IconProps) {
  const uid = useId();
  const bg = `jb-bg-${uid}`;
  const gl = `jb-gl-${uid}`;
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 32 32"
      fill="none"
      style={{ display: "inline-block", flexShrink: 0 }}
      {...rest}
    >
      <defs>
        <linearGradient id={bg} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="#1a8cff" />
          <stop offset="55%" stopColor="#0a6ef0" />
          <stop offset="100%" stopColor="#0045b8" />
        </linearGradient>
        <linearGradient id={gl} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="#ffffff" />
          <stop offset="100%" stopColor="#cfe4ff" />
        </linearGradient>
      </defs>
      {/* squircle (approximated via rounded rect) */}
      <rect
        x="1.25"
        y="1.25"
        width="29.5"
        height="29.5"
        rx="6.75"
        fill={`url(#${bg})`}
      />
      {/* glossy top highlight */}
      <path
        d="M1.25 8.44 L1.25 8 A6.75 6.75 0 0 1 8 1.25 L24 1.25 A6.75 6.75 0 0 1 30.75 8 L30.75 8.44 Z"
        fill="white"
        opacity="0.10"
      />
      {/* white play glyph */}
      <path
        d="M11.25 9.375 L20.75 16 L11.25 22.625 Z"
        fill={`url(#${gl})`}
        stroke={`url(#${gl})`}
        strokeWidth={2.25}
        strokeLinejoin="round"
      />
      {/* spring wave — Spring Boot */}
      <path
        d="M7.8 25 Q 10.3 24 12.8 25 T 17.8 25 T 22.8 25"
        stroke="#30d158"
        strokeWidth={1.75}
        fill="none"
        strokeLinecap="round"
      />
    </svg>
  );
}

/* Larger hero variant — same glyph, optimized for big display. */
export function HeroLogo({ size = 64, ...rest }: IconProps) {
  return <Logo size={size} {...rest} />;
}

export function Play({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M7 5 L19 12 L7 19 Z" fill="currentColor" stroke="none" />
    </Svg>
  );
}

export function Stop({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <rect x="6" y="6" width="12" height="12" fill="currentColor" stroke="none" />
    </Svg>
  );
}

export function Restart({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M5 12 A7 7 0 1 1 8 18" />
      <path d="M5 18 L5 13 L10 13" />
    </Svg>
  );
}

export function Settings({ size, ...rest }: IconProps) {
  // brutalist slider-cluster glyph
  return (
    <Svg size={size} {...rest}>
      <rect x="3" y="4" width="18" height="16" />
      <path d="M3 9 H21 M3 15 H21" />
      <circle cx="9" cy="6.5" r="1.1" fill="currentColor" stroke="none" />
      <circle cx="15" cy="12" r="1.1" fill="currentColor" stroke="none" />
      <circle cx="8" cy="17.5" r="1.1" fill="currentColor" stroke="none" />
    </Svg>
  );
}

export function Plus({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M12 4 V20 M4 12 H20" />
    </Svg>
  );
}

export function Folder({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M3 6 H10 L12 8 H21 V19 H3 Z" />
    </Svg>
  );
}

export function FolderOpen({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M3 6 H10 L12 8 H21 V11 H3 Z" />
      <path d="M3 11 L5 19 H21 L19 11" />
    </Svg>
  );
}

export function GitPull({ size, ...rest }: IconProps) {
  // downward arrow into a tray — "fetch"
  return (
    <Svg size={size} {...rest}>
      <path d="M12 4 V15" />
      <path d="M7 11 L12 16 L17 11" />
      <path d="M4 20 H20" />
      <circle cx="5" cy="6" r="1.4" fill="currentColor" stroke="none" />
      <circle cx="19" cy="6" r="1.4" fill="currentColor" stroke="none" />
    </Svg>
  );
}

export function GitPullRestart({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M12 3 V10" />
      <path d="M8 7 L12 11 L16 7" />
      <path d="M5 14 A7 7 0 1 1 5 19" />
      <path d="M5 19 L5 15 L9 15" />
    </Svg>
  );
}

export function Trash({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M4 7 H20" />
      <path d="M9 7 V4 H15 V7" />
      <path d="M6 7 L7 20 H17 L18 7" />
      <path d="M10 11 V16 M14 11 V16" />
    </Svg>
  );
}

export function CaretDown({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M5 9 L12 16 L19 9" />
    </Svg>
  );
}

export function CaretRight({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M9 5 L16 12 L9 19" />
    </Svg>
  );
}

export function Code({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M9 7 L4 12 L9 17" />
      <path d="M15 7 L20 12 L15 17" />
      <path d="M13 5 L11 19" />
    </Svg>
  );
}

export function More({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <circle cx="6" cy="12" r="1.3" fill="currentColor" stroke="none" />
      <circle cx="12" cy="12" r="1.3" fill="currentColor" stroke="none" />
      <circle cx="18" cy="12" r="1.3" fill="currentColor" stroke="none" />
    </Svg>
  );
}

export function Warning({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M12 3 L22 20 H2 Z" />
      <path d="M12 10 V14" />
      <circle cx="12" cy="17" r="0.6" fill="currentColor" stroke="none" />
    </Svg>
  );
}

export function Clear({ size, ...rest }: IconProps) {
  // bracketed X — "clear console"
  return (
    <Svg size={size} {...rest}>
      <path d="M4 5 V19 M20 5 V19" />
      <path d="M8 9 L16 15 M16 9 L8 15" />
    </Svg>
  );
}

export function Search({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <circle cx="11" cy="11" r="6" />
      <path d="M16 16 L21 21" />
    </Svg>
  );
}

export function Refresh({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M4 12 A8 8 0 1 1 7 18" />
      <path d="M4 18 L4 13 L9 13" />
    </Svg>
  );
}

export function ArrowDown({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M12 4 V19" />
      <path d="M6 14 L12 20 L18 14" />
    </Svg>
  );
}

export function File({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M6 3 H15 L19 7 V21 H6 Z" />
      <path d="M15 3 V7 H19" />
    </Svg>
  );
}

export function CheckSquare({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <rect x="3" y="3" width="18" height="18" />
      <path d="M7 12 L11 16 L17 8" />
    </Svg>
  );
}

export function Terminal({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <rect x="3" y="4" width="18" height="16" />
      <path d="M7 9 L10 12 L7 15" />
      <path d="M12 15 H17" />
    </Svg>
  );
}

/* status glyphs — small filled shapes for the card status node */
export function StatusDot({ color, live }: { color: string; live?: boolean }) {
  return (
    <span
      className={`status-node ${live ? "live" : ""}`}
      style={{ background: color, color }}
    />
  );
}

export function GitBranch({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <circle cx="6" cy="6" r="2.4" />
      <circle cx="6" cy="18" r="2.4" />
      <circle cx="18" cy="8" r="2.4" />
      <path d="M6 8.5 V15.5" />
      <path d="M8.3 7.6 C12 9 16 8.5 16 8.5" />
    </Svg>
  );
}

export function History({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M4 12 A8 8 0 1 1 8 18.5" />
      <path d="M4 19 L4 14 L9 14" />
      <path d="M12 7 V12 L15 14" />
    </Svg>
  );
}

export function Edit({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M4 20 L4 16.5 L16.5 4 L20 7.5 L7.5 20 Z" />
      <path d="M14.5 6 L18.5 10" />
    </Svg>
  );
}

export function Check({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M4 12 L10 18 L20 6" />
    </Svg>
  );
}

export function X({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M6 6 L18 18 M18 6 L6 18" />
    </Svg>
  );
}

export function ChevronLeft({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M14 5 L7 12 L14 19" />
    </Svg>
  );
}

export function Commit({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <circle cx="12" cy="12" r="2.6" fill="currentColor" stroke="none" />
      <path d="M12 3 V9.5 M12 14.5 V21" />
      <path d="M4 12 H9.5 M14.5 12 H20" />
    </Svg>
  );
}

export function Save({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M5 4 H15 L20 9 V20 H5 Z" />
      <path d="M8 4 V10 H16 V4" />
      <path d="M8 20 V14 H16 V20" />
    </Svg>
  );
}

export function Pause({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <rect x="6" y="5" width="4" height="14" fill="currentColor" stroke="none" />
      <rect x="14" y="5" width="4" height="14" fill="currentColor" stroke="none" />
    </Svg>
  );
}

export function Play2({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M7 5 L19 12 L7 19 Z" fill="currentColor" stroke="none" />
    </Svg>
  );
}

export function Moon({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M21 12.8 A9 9 0 1 1 11.2 3 A7 7 0 0 0 21 12.8 Z" />
    </Svg>
  );
}

export function Sun({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <circle cx="12" cy="12" r="4" />
      <path d="M12 2 V5 M12 19 V22 M4.2 4.2 L6.3 6.3 M17.7 17.7 L19.8 19.8 M2 12 H5 M19 12 H22 M4.2 19.8 L6.3 17.7 M17.7 6.3 L19.8 4.2" />
    </Svg>
  );
}

export function Layers({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M12 3 L21 8 L12 13 L3 8 Z" />
      <path d="M3 12 L12 17 L21 12" />
      <path d="M3 16 L12 21 L21 16" />
    </Svg>
  );
}

export function Broom({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <path d="M19 4 L17 6 L10 13 L7 13 L3 17 L4 20 L7 19 L11 15 L11 12 L18 5 L20 3 Z" />
      <path d="M5 17 L7 19" />
      <path d="M14 12 L16 14" />
    </Svg>
  );
}

export function Copy({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <rect x="3" y="3" width="13" height="13" rx="2" />
      <path d="M8 8 H19 V20 H8" />
    </Svg>
  );
}

export function Image({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <rect x="3" y="3" width="18" height="18" rx="2" />
      <circle cx="8.5" cy="8.5" r="1.5" />
      <path d="M21 15 L16 10 L5 21" />
    </Svg>
  );
}

export function Binary({ size, ...rest }: IconProps) {
  return (
    <Svg size={size} {...rest}>
      <rect x="3" y="3" width="18" height="18" rx="2" />
      <path d="M7 8 V12 M9 10 L5 10 M9 12 L5 12 M7 14 L7 16 M5 16 L9 16" />
      <path d="M14 7 V17 M12 7 L16 7 M12 17 L16 17 M14 12 L14 12" />
    </Svg>
  );
}
