// Monaco 代码编辑器组件
// 替换原 textarea+pre 双层叠加方案，内置语法高亮 / 行号 / 搜索 / 代码折叠

import { useCallback, useEffect, useRef } from "react";
import Editor from "@monaco-editor/react";
import type { editor } from "monaco-editor";
import { monaco, getMonacoTheme, setMonacoTheme } from "../monaco-setup";
import { getMonacoLang } from "../languages";

export interface MonacoCodeEditorHandle {
  /** 获取编辑器当前内容 */
  getValue: () => string;
  /** 设置编辑器内容（外部修改同步用） */
  setValue: (value: string) => void;
  /** 滚动到指定行 */
  revealLine: (line: number) => void;
  /** 获取编辑器实例 */
  getEditor: () => editor.IStandaloneCodeEditor | null;
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
}: Props) {
  const editorInstanceRef = useRef<editor.IStandaloneCodeEditor | null>(null);
  // 保存回调的 ref（避免每次 render 重建 editor addCommand）
  const onSaveRef = useRef(onSave);
  onSaveRef.current = onSave;

  /** 编辑器挂载 */
  const handleMount = useCallback(
    (ed: editor.IStandaloneCodeEditor) => {
      editorInstanceRef.current = ed;

      // 暴露 handle 方法给父组件
      if (editorRef) {
        (editorRef as MutableRef<MonacoCodeEditorHandle | null>).current = {
          getValue: () => ed.getValue() ?? "",
          setValue: (v: string) => {
            const model = ed.getModel();
            if (model) model.setValue(v);
          },
          revealLine: (line: number) => {
            ed.revealLineInCenter(line + 1);
          },
          getEditor: () => ed,
        };
      }

      // Ctrl+S 保存
      ed.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS, () => {
        onSaveRef.current();
      });
    },
    [editorRef]
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
