// Monaco 代码编辑器组件
// 替换原 textarea+pre 双层叠加方案，内置语法高亮 / 行号 / 搜索 / 代码折叠

import { useCallback, useEffect, useLayoutEffect, useRef } from "react";
import Editor from "@monaco-editor/react";
import type { editor } from "monaco-editor";
import { monaco, getMonacoTheme, setMonacoTheme } from "../monaco-setup";
import { getMonacoLang } from "../languages";

export interface MonacoCodeEditorHandle {
  /** 保存当前滚动位置/光标状态（外部内容同步前调用） */
  saveViewState: () => void;
  /** 恢复滚动位置/光标状态（外部内容同步后调用） */
  restoreViewState: () => void;
  /** 清除指定路径缓存的 viewState（标签关闭时调用，防止内存累积） */
  clearViewState: (path: string) => void;
}

interface Props {
  /** 文件路径（用于语言检测） */
  path: string;
  /** 文件内容 */
  value: string;
  /** 是否只读 */
  readonly: boolean;
  /** 磁盘原始 EOL 风格 */
  eol: "\n" | "\r\n";
  /** 内容变化回调 */
  onChange: (value: string) => void;
  /** Ctrl+S 保存回调 */
  onSave: () => void;
  /** ref 转发 */
  editorRef?: React.RefObject<MonacoCodeEditorHandle | null>;
  /** 编辑器实例就绪回调（供 Git gutter 等外部能力挂载） */
  onEditorReady?: (editor: editor.IStandaloneCodeEditor) => void;
}

interface MutableRef<T> {
  current: T;
}

export default function MonacoCodeEditor({
  path,
  value,
  readonly,
  eol,
  onChange,
  onSave,
  editorRef,
  onEditorReady,
}: Props) {
  const editorInstanceRef = useRef<editor.IStandaloneCodeEditor | null>(null);
  // 保存回调的 ref（避免每次 render 重建 editor addCommand）
  const onSaveRef = useRef(onSave);
  onSaveRef.current = onSave;

  // ---- 多标签滚动位置隔离 ----
  // 背景：@monaco-editor/react 自带的 saveViewState 存在缺陷——切换标签时，
  // 它会读「已更新为新 path」的闭包作为保存 key，导致把上一标签的滚动位置
  // 错误地覆盖到新标签名下，从而出现「滚动一个文件会带动其他已打开文件」。
  // 因此这里关闭库的 saveViewState，改为在组件内自行按 path 精确保存/恢复。
  //
  // 时序说明（React effect 执行顺序）：
  //   1. 本组件 useLayoutEffect（保存旧 tab 的 viewState）—— layout 阶段，先于库内部 effect
  //   2. 库 path effect（setModel 切换 model）
  //   3. 库 value effect（executeEdits 全量替换内容，会重置滚动位置/光标）
  //   4. 本组件 useEffect（restoreViewState 恢复滚动位置）—— 此时内容已同步，恢复有效
  // 为防御库 value effect 与本组件恢复 effect 的潜在时序偏差，恢复放在
  // requestAnimationFrame 中，确保浏览器绘制前完成恢复。
  const previousPathRef = useRef<string | null>(null);
  const viewStatesRef = useRef(new Map<string, editor.ICodeEditorViewState>());

  // 「保存」放在 useLayoutEffect：它先于库内部切换 model 的 useEffect 执行，
  // 此时 editor 仍挂在旧 model 上，saveViewState() 拿到的正是即将离开的标签的位置。
  // previousPathRef 在 handleMount 中已初始化为初始 path，因此首次切换时
  // prev !== null && prev !== path 成立，能正确保存初始标签的状态。
  useLayoutEffect(() => {
    const ed = editorInstanceRef.current;
    const prev = previousPathRef.current;
    if (!ed) return;
    if (prev !== null && prev !== path) {
      const state = ed.saveViewState();
      if (state) viewStatesRef.current.set(prev, state);
    }
  }, [path]);

  // 「恢复」放在 useEffect：此时库已完成 model 切换到新 path 并同步了 value，
  // 对当前 model 恢复对应标签保存过的滚动位置。
  // 用 requestAnimationFrame 延迟一帧，确保在库 value effect 的 executeEdits
  // 完成后再恢复，避免内容全量替换覆盖掉刚恢复的滚动位置。
  useEffect(() => {
    const ed = editorInstanceRef.current;
    if (!ed) {
      previousPathRef.current = path;
      return;
    }
    const saved = path ? viewStatesRef.current.get(path) : undefined;
    if (saved) {
      const raf = requestAnimationFrame(() => {
        ed.restoreViewState(saved);
      });
      previousPathRef.current = path;
      return () => cancelAnimationFrame(raf);
    }
    previousPathRef.current = path;
  }, [path]);

  /** 编辑器挂载 */
  const handleMount = useCallback(
    (ed: editor.IStandaloneCodeEditor) => {
      editorInstanceRef.current = ed;
      // 外部能力（如 Git gutter）挂载时机
      onEditorReady?.(ed);
      // 初始化 previousPathRef 为当前 path，使首次 tab 切换时保存逻辑
      // 走 prev !== null && prev !== path 分支，正确保存初始标签的 viewState。
      previousPathRef.current = path;

      // 暴露 handle 方法给父组件
      if (editorRef) {
        (editorRef as MutableRef<MonacoCodeEditorHandle | null>).current = {
          saveViewState: () => {
            const cur = previousPathRef.current;
            if (!cur) return;
            const state = ed.saveViewState();
            if (state) viewStatesRef.current.set(cur, state);
          },
          restoreViewState: () => {
            const cur = previousPathRef.current;
            if (!cur) return;
            const state = viewStatesRef.current.get(cur);
            if (state) {
              requestAnimationFrame(() => ed.restoreViewState(state));
            }
          },
          clearViewState: (path: string) => {
            viewStatesRef.current.delete(path);
          },
        };
      }

      // Ctrl+S 保存
      ed.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS, () => {
        onSaveRef.current();
      });
    },
    [editorRef, path, onEditorReady]
  );

  // 监听主题切换
  useEffect(() => {
    const observer = new MutationObserver(() => {
      setMonacoTheme();
    });
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
    return () => observer.disconnect();
  }, []);

  // EOL 设置：切换文件或 EOL 变化时更新 model
  useEffect(() => {
    const ed = editorInstanceRef.current;
    if (!ed) return;
    const model = ed.getModel();
    if (!model) return;
    const monacoEol = eol === "\r\n" ? monaco.editor.EndOfLineSequence.CRLF : monaco.editor.EndOfLineSequence.LF;
    model.setEOL(monacoEol);
  }, [eol, path]);

  // 组件卸载时清理
  useEffect(() => {
    return () => {
      if (editorRef) (editorRef as MutableRef<MonacoCodeEditorHandle | null>).current = null;
    };
  }, [editorRef]);

  return (
    <Editor
      height="100%"
      language={getMonacoLang(path)}
      value={value}
      theme={getMonacoTheme()}
      onMount={handleMount}
      onChange={(v) => onChange(v ?? "")}
      saveViewState={false}
      loading={<div style={{ padding: 40, textAlign: "center", color: "#a1a1a6" }}>加载编辑器…</div>}
      options={{
        readOnly: readonly,
        fontSize: 13,
        fontFamily: "var(--font-mono)",
        lineHeight: 22,
        lineNumbersMinChars: 4,
        glyphMargin: false,
        lineNumbers: "on",
        minimap: { enabled: !readonly },
        folding: true,
        renderWhitespace: "selection",
        scrollBeyondLastLine: false,
        wordWrap: "off",
        tabSize: 2,
        insertSpaces: true,
        automaticLayout: true,
        scrollbar: {
          verticalScrollbarSize: 10,
          horizontalScrollbarSize: 10,
          useShadows: false,
        },
        padding: { top: 12, bottom: 12 },
        smoothScrolling: true,
        cursorBlinking: "smooth",
        cursorSmoothCaretAnimation: "on",
        renderLineHighlight: "line",
        roundedSelection: true,
        selectionHighlight: true,
        occurrencesHighlight: "singleFile",
        guides: {
          indentation: true,
          bracketPairs: true,
        },
        bracketPairColorization: { enabled: true },
        stickyScroll: { enabled: true },
        fixedOverflowWidgets: true,
        overviewRulerBorder: false,
      }}
    />
  );
}
