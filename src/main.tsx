import React from "react";
import ReactDOM from "react-dom/client";
import {App as AntApp, Button, ConfigProvider, theme} from "antd";
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
            color: "#1d1d1f",
            background: "#f5f5f7",
            height: "100vh",
            fontFamily:
              '-apple-system, BlinkMacSystemFont, "SF Pro Text", "Helvetica Neue", "PingFang SC", "Segoe UI", sans-serif',
            overflow: "auto",
          }}
        >
          <h2 style={{ marginBottom: 16, fontWeight: 600, letterSpacing: "-0.02em" }}>
            渲染崩溃
          </h2>
          <pre
            style={{
              whiteSpace: "pre-wrap",
              marginBottom: 16,
              color: "#ff3b30",
              fontFamily: '"SF Mono", "JetBrains Mono", ui-monospace, monospace',
              fontSize: 12,
            }}
          >
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
      algorithm: theme.defaultAlgorithm,
      token: {
        colorPrimary: "#0071e3",
        colorBgContainer: "#ffffff",
        colorBgElevated: "#ffffff",
        colorBgBase: "#f5f5f7",
        colorText: "#1d1d1f",
        colorTextSecondary: "#6e6e73",
        colorTextTertiary: "#86868b",
        colorBorder: "#d2d2d7",
        colorBorderSecondary: "#e8e8ed",
        colorSuccess: "#34c759",
        colorWarning: "#ff9500",
        colorError: "#ff3b30",
        colorInfo: "#5ac8fa",
        borderRadius: 10,
        fontSize: 14,
        fontFamily:
          '-apple-system, BlinkMacSystemFont, "SF Pro Display", "SF Pro Text", "Helvetica Neue", "PingFang SC", "Microsoft YaHei", "Segoe UI", sans-serif',
        wireframe: false,
      },
      components: {
        Modal: { contentBg: "#ffffff", headerBg: "#ffffff" },
        Drawer: { colorBgElevated: "#ffffff" },
        Tabs: { cardBg: "transparent", titleFontSize: 13 },
        Segmented: { itemColor: "#6e6e73" },
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
