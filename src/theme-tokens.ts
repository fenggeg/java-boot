/** 与 styles/tokens.css 对齐的 antd Design Token — 单一事实来源 */
export const lightAntdTokens = {
  colorPrimary: "#0071e3",
  colorBgContainer: "#ffffff",
  colorBgElevated: "#ffffff",
  colorBgBase: "#f5f5f7",
  colorText: "#1d1d1f",
  colorTextSecondary: "#6e6e73",
  colorTextTertiary: "#86868b",
  colorBorder: "rgba(0, 0, 0, 0.14)",
  colorBorderSecondary: "rgba(0, 0, 0, 0.06)",
  colorSuccess: "#34c759",
  colorWarning: "#ff9500",
  colorError: "#ff3b30",
  colorInfo: "#5ac8fa",
} as const;

export const darkAntdTokens = {
  colorPrimary: "#0a84ff",
  colorBgContainer: "#1c1c1e",
  colorBgElevated: "#2c2c2e",
  colorBgBase: "#000000",
  colorText: "#f5f5f7",
  colorTextSecondary: "#98989d",
  colorTextTertiary: "#8e8e93",
  colorBorder: "rgba(255, 255, 255, 0.2)",
  colorBorderSecondary: "rgba(255, 255, 255, 0.08)",
  colorSuccess: "#30d158",
  colorWarning: "#ff9f0a",
  colorError: "#ff453a",
  colorInfo: "#64d2ff",
} as const;

export const sharedAntdTokens = {
  borderRadius: 10,
  borderRadiusLG: 14,
  borderRadiusSM: 6,
  fontSize: 14,
  controlHeight: 32,
  fontFamily:
    '-apple-system, BlinkMacSystemFont, "SF Pro Display", "SF Pro Text", "Helvetica Neue", "PingFang SC", "Microsoft YaHei", "Segoe UI", sans-serif',
  wireframe: false,
} as const;
