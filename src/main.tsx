// 入口文件：组件不导出、由 ReactDOM 挂载，不参与 HMR fast-refresh，
// 豁免 react-refresh/only-export-components 检查（入口文件属常规例外）。
/* eslint-disable react-refresh/only-export-components */
import {useEffect} from "react";
import ReactDOM from "react-dom/client";
import {App as AntApp, ConfigProvider, theme} from "antd";
import zhCN from "antd/locale/zh_CN";
import {useThemeStore} from "./theme";
import {darkAntdTokens, lightAntdTokens, sharedAntdTokens} from "./theme-tokens";
import App from "./App";
import ErrorBoundary from "./components/ErrorBoundary";
import "./styles.css";

// 禁用 webview 内容区右键菜单（打包后不暴露浏览器上下文菜单）
// 拦截浏览器全局快捷键，避免在 Tauri WebView 中触发刷新 / 查找等原生行为。
// 所有模块顶层 DOM 操作统一移入 useEffect，确保在 React 生命周期内执行。
function useGlobalKeyGuard() {
  useEffect(() => {
    const onContextMenu = (e: MouseEvent) => e.preventDefault();
    document.addEventListener("contextmenu", onContextMenu);

    const onKeyDown = (e: KeyboardEvent) => {
      const k = e.key.toLowerCase();
      const ctrl = e.ctrlKey || e.metaKey;

      // 刷新：Ctrl+R / Cmd+R / Ctrl+Shift+R / F5
      if ((ctrl && k === "r") || e.key === "F5") {
        e.preventDefault();
        e.stopPropagation();
        return;
      }

      // 查找：Ctrl+F — 仅 preventDefault，不阻断传播
      if (ctrl && !e.altKey && k === "f") {
        e.preventDefault();
        return;
      }

      // 页面缩放：Ctrl+Plus / Ctrl+Minus / Ctrl+0
      if (ctrl && (k === "+" || k === "-" || k === "0" || k === "=")) {
        e.preventDefault();
        e.stopPropagation();
        return;
      }

      // 开发者工具
      if ((ctrl && e.shiftKey && k === "i") || e.key === "F12") {
        e.preventDefault();
        e.stopPropagation();
        return;
      }

      // Backspace 后退（非输入控件焦点时）
      if (
        e.key === "Backspace" &&
        !e.altKey &&
        !e.ctrlKey &&
        !e.metaKey
      ) {
        const tag = (document.activeElement?.tagName ?? "").toLowerCase();
        const isEditable =
          tag === "input" ||
          tag === "textarea" ||
          (document.activeElement as HTMLElement)?.isContentEditable;
        if (!isEditable) {
          e.preventDefault();
          e.stopPropagation();
        }
      }
    };
    window.addEventListener("keydown", onKeyDown, true);

    return () => {
      document.removeEventListener("contextmenu", onContextMenu);
      window.removeEventListener("keydown", onKeyDown, true);
    };
  }, []);
}

function ThemedApp() {
  const mode = useThemeStore((s) => s.mode);
  const isDark = mode === "dark";

  // 全局快捷键拦截 + 右键菜单禁用
  useGlobalKeyGuard();

  // 同步 data-theme 到 <html> 供 CSS 变量切换
  if (typeof document !== "undefined") {
    document.documentElement.setAttribute("data-theme", mode);
  }

  return (
    <ConfigProvider
      locale={zhCN}
      theme={{
        algorithm: isDark ? theme.darkAlgorithm : theme.defaultAlgorithm,
        token: {
          ...(isDark ? darkAntdTokens : lightAntdTokens),
          ...sharedAntdTokens,
        },
        components: {
          Modal: {
            contentBg: isDark ? darkAntdTokens.colorBgContainer : lightAntdTokens.colorBgContainer,
            headerBg: isDark ? darkAntdTokens.colorBgContainer : lightAntdTokens.colorBgContainer,
          },
          Drawer: {
            colorBgElevated: isDark ? darkAntdTokens.colorBgContainer : lightAntdTokens.colorBgContainer,
          },
          Tabs: { cardBg: "transparent", titleFontSize: 13 },
          Segmented: {
            itemColor: isDark
              ? darkAntdTokens.colorTextSecondary
              : lightAntdTokens.colorTextSecondary,
          },
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
