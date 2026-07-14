import React from "react";
import ReactDOM from "react-dom/client";
import { ConfigProvider, theme, App as AntApp } from "antd";
import zhCN from "antd/locale/zh_CN";
import App from "./App";
import "./styles.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <ConfigProvider
    locale={zhCN}
    theme={{
      algorithm: theme.darkAlgorithm,
      token: {
        colorPrimary: "#a3e635",
        colorBgContainer: "#16191d",
        colorBgElevated: "#1c2025",
        colorBgBase: "#0a0b0d",
        colorText: "#e6e8eb",
        colorTextSecondary: "#9ba1a8",
        colorTextTertiary: "#626771",
        colorBorder: "#2e333c",
        colorBorderSecondary: "#23272e",
        colorSuccess: "#a3e635",
        colorWarning: "#fbbf24",
        colorError: "#f87171",
        colorInfo: "#22d3ee",
        borderRadius: 2,
        fontSize: 13,
        fontFamily:
          '"IBM Plex Sans", -apple-system, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif',
        wireframe: false,
      },
      components: {
        Modal: { contentBg: "#111316", headerBg: "#111316" },
        Drawer: { colorBgElevated: "#111316" },
        Tabs: { cardBg: "#16191d", titleFontSize: 11 },
        Segmented: { itemColor: "#9ba1a8" },
      },
    }}
  >
    <AntApp>
      <App />
    </AntApp>
  </ConfigProvider>
);
