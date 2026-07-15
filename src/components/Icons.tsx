/**
 * JavaBoot Launcher — icon set
 * Clean SF-Symbol-style line glyphs with round caps/joins.
 * All icons inherit `currentColor`; 24×24 viewBox unless noted.
 */
import type {ReactNode, SVGProps} from "react";

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

/* ── Brand mark: iOS-style squircle app icon ─────────────────
   Rounded squircle with blue gradient + white play glyph,
   echoing the macOS/iOS app icon idiom. */
export function Logo({ size = 24, ...rest }: IconProps) {
  const uid = "jblogo";
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
        <linearGradient id={uid} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="#0a84ff" />
          <stop offset="100%" stopColor="#0051d5" />
        </linearGradient>
      </defs>
      {/* squircle (approximated via rounded rect) */}
      <rect
        x="1"
        y="1"
        width="30"
        height="30"
        rx="8"
        fill={`url(#${uid})`}
      />
      {/* glossy top highlight */}
      <rect
        x="1"
        y="1"
        width="30"
        height="15"
        rx="8"
        fill="white"
        opacity="0.12"
      />
      {/* white play glyph */}
      <path
        d="M12.5 10 L22 16 L12.5 22 Z"
        fill="#ffffff"
        stroke="#ffffff"
        strokeWidth={1.4}
        strokeLinejoin="round"
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
