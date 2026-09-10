// Monaco Editor 初始化配置：Worker 设置 + 自定义主题
// 主题配色对齐工程极简（亮色 / 暗色），与 tokens.css 灰阶 + #0070f3 一致

import * as monaco from "monaco-editor";
import { loader } from "@monaco-editor/react";
import editorWorker from "monaco-editor/esm/vs/editor/editor.worker?worker";
import jsonWorker from "monaco-editor/esm/vs/language/json/json.worker?worker";
import cssWorker from "monaco-editor/esm/vs/language/css/css.worker?worker";
import htmlWorker from "monaco-editor/esm/vs/language/html/html.worker?worker";
import tsWorker from "monaco-editor/esm/vs/language/typescript/ts.worker?worker";

// 关键：让 @monaco-editor/react 的 loader 使用本地 ESM 打包的 monaco 实例，
// 而不是默认的 jsdelivr CDN（CSP 会拦截 CDN 脚本，导致首次打开编辑器永远加载不出来）。
// loader.config({ monaco }) 是官方支持的注入方式：init() 检测到 monaco 已提供，
// 直接 resolve 本地实例，完全跳过 CDN script 注入。
loader.config({ monaco });

// Vite 环境下配置 Worker 环境
// WebView2 支持 Worker，经 Vite 打包后为同源脚本；
// 按语言标签分发对应 worker，使 json/css/html/typescript 获得校验 / 补全能力
self.MonacoEnvironment = {
  getWorker(_: unknown, label: string) {
    if (label === "json") return new jsonWorker();
    if (label === "css" || label === "scss" || label === "less")
      return new cssWorker();
    if (label === "html" || label === "handlebars" || label === "razor")
      return new htmlWorker();
    if (label === "typescript" || label === "javascript") return new tsWorker();
    return new editorWorker();
  },
};

// ================================================================
// 自定义主题：工程极简（亮色 / 暗色）
// ================================================================

monaco.editor.defineTheme("jb-light", {
  base: "vs",
  inherit: true,
  rules: [
    { token: "comment", foreground: "737373", fontStyle: "italic" },
    { token: "punctuation", foreground: "525252" },
    { token: "number", foreground: "be123c" },
    { token: "string", foreground: "15803d" },
    { token: "keyword", foreground: "0070f3" },
    { token: "function", foreground: "7c3aed" },
    { token: "type", foreground: "7c3aed" },
    { token: "variable", foreground: "b45309" },
    { token: "operator", foreground: "525252" },
    { token: "delimiter", foreground: "737373" },
    { token: "attribute.name", foreground: "15803d" },
    { token: "attribute.value", foreground: "0070f3" },
    { token: "tag", foreground: "be123c" },
    { token: "property", foreground: "be123c" },
    { token: "constant", foreground: "be123c" },
    { token: "annotation", foreground: "7c3aed" },
    { token: "annotation.identifier", foreground: "7c3aed" },
  ],
  colors: {
    "editor.background": "#ffffff",
    "editor.foreground": "#0a0a0a",
    "editorLineNumber.foreground": "#a3a3a3",
    "editorLineNumber.activeForeground": "#0a0a0a",
    "editor.selectionBackground": "#0070f322",
    "editor.lineHighlightBackground": "#0070f30a",
    "editorCursor.foreground": "#0070f3",
    "editorWidget.background": "#ffffff",
    "editorWidget.border": "#e5e5e5",
    "editorSuggestWidget.background": "#ffffff",
    "editorSuggestWidget.border": "#e5e5e5",
    "editorSuggestWidget.selectedBackground": "#0070f315",
    "editorHoverWidget.background": "#ffffff",
    "editorHoverWidget.border": "#e5e5e5",
    "scrollbarSlider.background": "#a3a3a340",
    "scrollbarSlider.hoverBackground": "#a3a3a360",
    "scrollbarSlider.activeBackground": "#a3a3a380",
    "minimap.background": "#ffffff",
    "editorGutter.background": "#ffffff",
  },
});

monaco.editor.defineTheme("jb-dark", {
  base: "vs-dark",
  inherit: true,
  rules: [
    { token: "comment", foreground: "737373", fontStyle: "italic" },
    { token: "punctuation", foreground: "a1a1a1" },
    { token: "number", foreground: "f472b6" },
    { token: "string", foreground: "4ade80" },
    { token: "keyword", foreground: "3291ff" },
    { token: "function", foreground: "fbbf24" },
    { token: "type", foreground: "fbbf24" },
    { token: "variable", foreground: "fde047" },
    { token: "operator", foreground: "d4d4d4" },
    { token: "delimiter", foreground: "a1a1a1" },
    { token: "attribute.name", foreground: "4ade80" },
    { token: "attribute.value", foreground: "3291ff" },
    { token: "tag", foreground: "f472b6" },
    { token: "property", foreground: "f472b6" },
    { token: "constant", foreground: "f472b6" },
    { token: "annotation", foreground: "fbbf24" },
    { token: "annotation.identifier", foreground: "fbbf24" },
  ],
  colors: {
    "editor.background": "#111111",
    "editor.foreground": "#ededed",
    "editorLineNumber.foreground": "#525252",
    "editorLineNumber.activeForeground": "#a1a1a1",
    "editor.selectionBackground": "#3291ff33",
    "editor.lineHighlightBackground": "#ffffff0a",
    "editorCursor.foreground": "#3291ff",
    "editorWidget.background": "#1a1a1a",
    "editorWidget.border": "#262626",
    "editorSuggestWidget.background": "#1a1a1a",
    "editorSuggestWidget.border": "#262626",
    "editorSuggestWidget.selectedBackground": "#3291ff22",
    "editorHoverWidget.background": "#1a1a1a",
    "editorHoverWidget.border": "#262626",
    "scrollbarSlider.background": "#52525240",
    "scrollbarSlider.hoverBackground": "#52525260",
    "scrollbarSlider.activeBackground": "#52525280",
    "minimap.background": "#111111",
    "editorGutter.background": "#111111",
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
