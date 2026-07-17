import ReactDOM from "react-dom/client";
import {App as AntApp, ConfigProvider, theme} from "antd";
import zhCN from "antd/locale/zh_CN";
import App from "./App";
import ErrorBoundary from "./components/ErrorBoundary";
import "./styles.css";

// 禁用 webview 内容区右键菜单（打包后不暴露浏览器上下文菜单）
document.addEventListener("contextmenu", (e) => e.preventDefault());

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
