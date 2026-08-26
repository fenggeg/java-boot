import ReactDOM from "react-dom/client";
import {App as AntApp, ConfigProvider, theme} from "antd";
import zhCN from "antd/locale/zh_CN";
import {useThemeStore} from "./theme";
import App from "./App";
import ErrorBoundary from "./components/ErrorBoundary";
import "./styles.css";

// 禁用 webview 内容区右键菜单（打包后不暴露浏览器上下文菜单）
document.addEventListener("contextmenu", (e) => e.preventDefault());

// 关闭浏览器默认的全局查找（Ctrl+F / Cmd+F）：由文件编辑器内置搜索替代。
// 仅 preventDefault 不阻断传播——FilePanel 的监听器仍可收到并打开编辑器搜索。
window.addEventListener(
  "keydown",
  (e) => {
    if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === "f") {
      e.preventDefault();
    }
  },
  true
);

const lightTokens = {
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
};

const darkTokens = {
  colorPrimary: "#0a84ff",
  colorBgContainer: "#1c1c1e",
  colorBgElevated: "#2c2c2e",
  colorBgBase: "#000000",
  colorText: "#f5f5f7",
  colorTextSecondary: "#98989d",
  colorTextTertiary: "#8e8e93",
  colorBorder: "#38383a",
  colorBorderSecondary: "#2c2c2e",
  colorSuccess: "#30d158",
  colorWarning: "#ff9f0a",
  colorError: "#ff453a",
  colorInfo: "#64d2ff",
};

function ThemedApp() {
  const mode = useThemeStore((s) => s.mode);
  const isDark = mode === "dark";

  // 同步 data-theme 到 <html> 供 CSS 变量切换
  if (typeof document !== "undefined") {
    document.documentElement.setAttribute("data-theme", mode);
  }

  const sharedTokens = {
    borderRadius: 10,
    fontSize: 14,
    fontFamily:
      '-apple-system, BlinkMacSystemFont, "SF Pro Display", "SF Pro Text", "Helvetica Neue", "PingFang SC", "Microsoft YaHei", "Segoe UI", sans-serif',
    wireframe: false,
  };

  return (
    <ConfigProvider
      locale={zhCN}
      theme={{
        algorithm: isDark ? theme.darkAlgorithm : theme.defaultAlgorithm,
        token: { ...(isDark ? darkTokens : lightTokens), ...sharedTokens },
        components: {
          Modal: {
            contentBg: isDark ? "#1c1c1e" : "#ffffff",
            headerBg: isDark ? "#1c1c1e" : "#ffffff",
          },
          Drawer: {
            colorBgElevated: isDark ? "#1c1c1e" : "#ffffff",
          },
          Tabs: { cardBg: "transparent", titleFontSize: 13 },
          Segmented: { itemColor: isDark ? "#98989d" : "#6e6e73" },
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
}

ReactDOM.createRoot(document.getElementById("root")!).render(<ThemedApp />);
