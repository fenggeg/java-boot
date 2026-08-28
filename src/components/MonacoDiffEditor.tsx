// Monaco DiffEditor 封装
// 用于 Git Diff 面板，替换原 <pre> 手写 diff 着色
// 左侧=原始版本(只读) 右侧=修改后版本(可编辑)，由 Monaco 内置 diff 算法高亮差异

import { useCallback, useEffect, useRef } from "react";
import { DiffEditor } from "@monaco-editor/react";
import type { editor } from "monaco-editor";
import { getMonacoTheme, setMonacoTheme } from "../monaco-setup";
import { getMonacoLang } from "../languages";

export interface MonacoDiffEditorHandle {
  /** 获取右侧（modified）编辑器当前内容 */
  getModifiedValue: () => string;
  /** 获取右侧编辑器实例 */
  getModifiedEditor: () => editor.IStandaloneCodeEditor | null;
}

interface Props {
  /** 文件路径（用于语言检测） */
  path: string;
  /** 原始版本内容（左侧）；null 表示新增文件（左侧空） */
  original: string | null;
  /** 修改后版本内容（右侧） */
  modified: string;
  /** 右侧是否可编辑 */
  editable?: boolean;
  /** 右侧内容变化回调 */
  onModifiedChange?: (value: string) => void;
  /** ref 转发 */
  diffEditorRef?: React.RefObject<MonacoDiffEditorHandle | null>;
  /** 自定义高度 */
  height?: string;
}

interface MutableRef<T> {
  current: T;
}

export default function MonacoDiffEditor({
  path,
  original,
  modified,
  editable = false,
  onModifiedChange,
  diffEditorRef,
  height = "100%",
}: Props) {
  const modifiedEditorRef = useRef<editor.IStandaloneCodeEditor | null>(null);
  const onModifiedChangeRef = useRef(onModifiedChange);
  onModifiedChangeRef.current = onModifiedChange;

  const handleMount = useCallback(
    (diffEditor: editor.IStandaloneDiffEditor) => {
      const modified = diffEditor.getModifiedEditor();
      modifiedEditorRef.current = modified;

      if (diffEditorRef) {
        (diffEditorRef as MutableRef<MonacoDiffEditorHandle | null>).current = {
          getModifiedValue: () => modified.getValue() ?? "",
          getModifiedEditor: () => modified,
        };
      }

      if (editable && onModifiedChangeRef.current) {
        modified.onDidChangeModelContent(() => {
          onModifiedChangeRef.current?.(modified.getValue() ?? "");
        });
      }
    },
    [diffEditorRef, editable]
  );

  // 主题切换监听
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

  // 卸载清理
  useEffect(() => {
    return () => {
      if (diffEditorRef)
        (diffEditorRef as MutableRef<MonacoDiffEditorHandle | null>).current = null;
    };
  }, [diffEditorRef]);

  const lang = getMonacoLang(path);

  return (
    <DiffEditor
      height={height}
      language={lang}
      original={original ?? ""}
      modified={modified}
      theme={getMonacoTheme()}
      onMount={handleMount}
      loading={
        <div style={{ padding: 40, textAlign: "center", color: "#a1a1a6" }}>
          加载差异编辑器…
        </div>
      }
      options={{
        readOnly: !editable,
        renderSideBySide: true,
        originalEditable: false,
        diffWordWrap: "off",
        renderOverviewRuler: false,
        scrollbar: {
          verticalScrollbarSize: 10,
          horizontalScrollbarSize: 10,
          useShadows: false,
        },
        // 两侧编辑器共用选项
        fontSize: 13,
        fontFamily: "var(--font-mono)",
        lineHeight: 22,
        lineNumbersMinChars: 4,
        glyphMargin: false,
        minimap: { enabled: false },
        folding: true,
        renderWhitespace: "selection",
        scrollBeyondLastLine: false,
        wordWrap: "off",
        automaticLayout: true,
        padding: { top: 10, bottom: 10 },
        smoothScrolling: true,
        stickyScroll: { enabled: false },
        fixedOverflowWidgets: true,
        overviewRulerBorder: false,
        ...(editable
          ? {}
          : {
              cursorBlinking: "solid" as const,
              renderLineHighlight: "none" as const,
              selectionHighlight: false,
              occurrencesHighlight: "off" as const,
            }),
      }}
    />
  );
}
