// Monaco Editor 初始化配置：Worker 设置 + 自定义主题
// 主题配色对齐原有 Prism Xcode 风格（亮色 / 暗色）

import * as monaco from "monaco-editor";
// @ts-expect-error — Vite ?worker 后缀在 tsc 下无类型声明
import editorWorker from "monaco-editor/esm/vs/editor/editor.worker?worker";

// Vite 环境下配置 Worker 入口
// WebView2 支持 Worker，经 Vite 打包后为同源脚本
self.MonacoEnvironment = {
  getWorker() {
    return new editorWorker();
  },
};

// ================================================================
// 自定义主题：Xcode 风格（亮色 / 暗色）
// token 颜色与原 Prism 配色保持一致
// ================================================================

monaco.editor.defineTheme("jb-light", {
  base: "vs",
  inherit: true,
  rules: [
    { token: "comment", foreground: "6e6e73", fontStyle: "italic" },
    { token: "punctuation", foreground: "48484a" },
    { token: "number", foreground: "c93400" },
    { token: "string", foreground: "248a3d" },
    { token: "keyword", foreground: "0071e3" },
    { token: "function", foreground: "ad3da4" },
    { token: "type", foreground: "ad3da4" },
    { token: "variable", foreground: "e28d0a" },
    { token: "operator", foreground: "6f42c1" },
    { token: "delimiter", foreground: "48484a" },
    { token: "attribute.name", foreground: "248a3d" },
    { token: "attribute.value", foreground: "0071e3" },
    { token: "tag", foreground: "c93400" },
    { token: "property", foreground: "c93400" },
    { token: "constant", foreground: "c93400" },
    { token: "annotation", foreground: "ad3da4" },
    { token: "annotation.identifier", foreground: "ad3da4" },
  ],
  colors: {
    "editor.background": "#ffffff",
    "editor.foreground": "#1d1d1f",
    "editorLineNumber.foreground": "#a1a1a6",
    "editorLineNumber.activeForeground": "#1d1d1f",
    "editor.selectionBackground": "#0071e322",
    "editor.lineHighlightBackground": "#0071e30a",
    "editorCursor.foreground": "#0071e3",
    "editorWidget.background": "#ffffff",
    "editorWidget.border": "#d2d2d7",
    "editorSuggestWidget.background": "#ffffff",
    "editorSuggestWidget.border": "#d2d2d7",
    "editorSuggestWidget.selectedBackground": "#0071e315",
    "editorHoverWidget.background": "#ffffff",
    "editorHoverWidget.border": "#d2d2d7",
    "scrollbarSlider.background": "#a1a1a640",
    "scrollbarSlider.hoverBackground": "#a1a1a660",
    "scrollbarSlider.activeBackground": "#a1a1a680",
    "minimap.background": "#ffffff",
    "editorGutter.background": "#ffffff",
  },
});

monaco.editor.defineTheme("jb-dark", {
  base: "vs-dark",
  inherit: true,
  rules: [
    { token: "comment", foreground: "7c7c80", fontStyle: "italic" },
    { token: "punctuation", foreground: "98989d" },
    { token: "number", foreground: "ff7ab2" },
    { token: "string", foreground: "69e4a6" },
    { token: "keyword", foreground: "4fa8ff" },
    { token: "function", foreground: "ff9f0a" },
    { token: "type", foreground: "ff9f0a" },
    { token: "variable", foreground: "ffd60a" },
    { token: "operator", foreground: "d2a8ff" },
    { token: "delimiter", foreground: "98989d" },
    { token: "attribute.name", foreground: "69e4a6" },
    { token: "attribute.value", foreground: "4fa8ff" },
    { token: "tag", foreground: "ff7ab2" },
    { token: "property", foreground: "ff7ab2" },
    { token: "constant", foreground: "ff7ab2" },
    { token: "annotation", foreground: "ff9f0a" },
    { token: "annotation.identifier", foreground: "ff9f0a" },
  ],
  colors: {
    "editor.background": "#1c1c1e",
    "editor.foreground": "#f5f5f7",
    "editorLineNumber.foreground": "#48484a",
    "editorLineNumber.activeForeground": "#98989d",
    "editor.selectionBackground": "#4fa8ff33",
    "editor.lineHighlightBackground": "#ffffff0a",
    "editorCursor.foreground": "#4fa8ff",
    "editorWidget.background": "#2c2c2e",
    "editorWidget.border": "#3a3a3c",
    "editorSuggestWidget.background": "#2c2c2e",
    "editorSuggestWidget.border": "#3a3a3c",
    "editorSuggestWidget.selectedBackground": "#4fa8ff22",
    "editorHoverWidget.background": "#2c2c2e",
    "editorHoverWidget.border": "#3a3a3c",
    "scrollbarSlider.background": "#63636640",
    "scrollbarSlider.hoverBackground": "#63636660",
    "scrollbarSlider.activeBackground": "#63636680",
    "minimap.background": "#1c1c1e",
    "editorGutter.background": "#1c1c1e",
  },
});

/** 根据当前 data-theme 属性返回 Monaco 主题名 */
export function getMonacoTheme(): string {
  return document.documentElement.getAttribute("data-theme") === "dark"
    ? "jb-dark"
    : "jb-light";
}

/** 切换 Monaco 主题（监听 data-theme 变化时调用） */
export function setMonacoTheme(): void {
  monaco.editor.setTheme(getMonacoTheme());
}

export { monaco };
