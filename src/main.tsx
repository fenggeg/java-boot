import React from "react";
import ReactDOM from "react-dom/client";
import { ConfigProvider, theme, App as AntApp, Button } from "antd";
import zhCN from "antd/locale/zh_CN";
import App from "./App";
import "./styles.css";

// 简易 ErrorBoundary：捕获子树渲染异常，避免整页白屏崩溃
class ErrorBoundary extends React.Component<
  { children: React.ReactNode },
  { error: Error | null }
> {
  state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo) {
    console.error("渲染异常:", error, info);
  }

  render() {
    if (this.state.error) {
      return (
        <div
          style={{
            padding: 32,
            color: "#f87171",
            background: "#0a0b0d",
            height: "100vh",
            fontFamily: "monospace",
            overflow: "auto",
          }}
        >
          <h2 style={{ marginBottom: 16 }}>// 渲染崩溃</h2>
          <pre style={{ whiteSpace: "pre-wrap", marginBottom: 16 }}>
            {this.state.error.message}
            {"\n\n"}
            {this.state.error.stack}
          </pre>
          <Button
            type="primary"
            size="small"
            onClick={() => this.setState({ error: null })}
          >
            重试
          </Button>
        </div>
      );
    }
    return this.props.children;
  }
}

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
      <ErrorBoundary>
        <App />
      </ErrorBoundary>
    </AntApp>
  </ConfigProvider>
);
