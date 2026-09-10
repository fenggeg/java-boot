/** 与 styles/tokens.css 对齐的 antd Design Token — 单一事实来源（工程极简） */
export const lightAntdTokens = {
  colorPrimary: "#0070f3",
  colorBgContainer: "#ffffff",
  colorBgElevated: "#ffffff",
  colorBgBase: "#fafafa",
  colorText: "#0a0a0a",
  colorTextSecondary: "#525252",
  colorTextTertiary: "#737373",
  colorBorder: "#d4d4d4",
  colorBorderSecondary: "#e5e5e5",
  colorSuccess: "#16a34a",
  colorWarning: "#d97706",
  colorError: "#dc2626",
  colorInfo: "#0891b2",
} as const;

export const darkAntdTokens = {
  colorPrimary: "#3291ff",
  colorBgContainer: "#111111",
  colorBgElevated: "#1a1a1a",
  colorBgBase: "#0a0a0a",
  colorText: "#ededed",
  colorTextSecondary: "#a1a1a1",
  colorTextTertiary: "#737373",
  colorBorder: "#404040",
  colorBorderSecondary: "#262626",
  colorSuccess: "#22c55e",
  colorWarning: "#f59e0b",
  colorError: "#ef4444",
  colorInfo: "#22d3ee",
} as const;

export const sharedAntdTokens = {
  borderRadius: 4,
  borderRadiusLG: 6,
  borderRadiusSM: 4,
  fontSize: 13,
  controlHeight: 30,
  fontFamily:
    '"Inter", "Segoe UI", "PingFang SC", "Microsoft YaHei", system-ui, sans-serif',
  wireframe: false,
} as const;
