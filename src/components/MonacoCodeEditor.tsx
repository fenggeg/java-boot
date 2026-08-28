// Monaco 代码编辑器组件
// 替换原 textarea+pre 双层叠加方案，内置语法高亮 / 行号 / 搜索 / 代码折叠
// 对接 Git diff glyph margin 装饰 + 点击操作菜单

import { useCallback, useEffect, useRef } from "react";
import Editor from "@monaco-editor/react";
import type { editor, IRange } from "monaco-editor";
import { monaco, getMonacoTheme, setMonacoTheme } from "../monaco-setup";
import { getMonacoLang } from "../languages";
import type { LineKind } from "./FilePanel";

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
  /** Git diff 行标记（与行号对齐，0基） */
  lineKinds?: LineKind[] | null;
  /** 点击 glyph margin 装饰时回调（line 为 0 基行号） */
  onGutterClick?: (line: number) => void;
  /** 点击编辑器内容区（非 glyph margin）时回调（关闭弹出面板用） */
  onContentClick?: () => void;
  /** ref 转发 */
  editorRef?: React.RefObject<MonacoCodeEditorHandle | null>;
}

/** diff 标记 → glyph margin CSS class */
function diffClass(kind: LineKind): string {
  switch (kind) {
    case 1:
      return "jb-diff-mod";
    case 2:
      return "jb-diff-add";
    case 3:
      return "jb-diff-del";
    default:
      return "";
  }
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
  lineKinds,
  onGutterClick,
  onContentClick,
  editorRef,
}: Props) {
  const editorInstanceRef = useRef<editor.IStandaloneCodeEditor | null>(null);
  const decorationsRef = useRef<string[]>([]);
  // 保存回调的 ref（避免每次 render 重建 editor addCommand）
  // 回调的 ref（避免每次 render 重建 editor 事件监听）
  const onSaveRef = useRef(onSave);
  onSaveRef.current = onSave;
  const onGutterClickRef = useRef(onGutterClick);
  onGutterClickRef.current = onGutterClick;
  const onContentClickRef = useRef(onContentClick);
  onContentClickRef.current = onContentClick;

  /** 构建 glyph margin + 行内背景装饰 */
  const applyDiffDecorations = useCallback(
    (editor: editor.IStandaloneCodeEditor, kinds: LineKind[] | null) => {
      if (!kinds || !kinds.some((k) => k !== 0)) {
        if (decorationsRef.current.length > 0) {
          decorationsRef.current = editor.deltaDecorations(
            decorationsRef.current,
            []
          );
        }
        return;
      }
      const decos: editor.IModelDeltaDecoration[] = [];
      for (let i = 0; i < kinds.length; i++) {
        const k = kinds[i]!;
        if (k === 0) continue;
        const cls = diffClass(k);
        if (!cls) continue;
        const lineNumber = i + 1; // Monaco 行号 1 基
        const range: IRange = {
          startLineNumber: lineNumber,
          startColumn: 1,
          endLineNumber: lineNumber,
          endColumn: 1,
        };
        // glyph margin 标记（左缘色条）
        decos.push({
          range,
          options: {
            isWholeLine: true,
            glyphMarginClassName: `jb-glyph ${cls}`,
            glyphMarginHoverMessage: { value: "变更操作：跳转 / 撤销 / 历史" },
            // 行内背景色（修改=橙底，新增=绿底，删除不加行内色）
            className: k === 1 ? "jb-line-mod" : k === 2 ? "jb-line-add" : undefined,
          },
        });
      }
      decorationsRef.current = editor.deltaDecorations(
        decorationsRef.current,
        decos
      );
    },
    []
  );

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

      // glyph margin 点击：弹出操作菜单；点内容区：关闭弹出面板
      ed.onMouseDown((e) => {
        if (
          e.target.type === monaco.editor.MouseTargetType.GUTTER_GLYPH_MARGIN &&
          e.target.position
        ) {
          const line = e.target.position.lineNumber - 1; // 转 0 基
          onGutterClickRef.current?.(line);
        } else {
          onContentClickRef.current?.();
        }
      });

      // 初始 diff 装饰
      applyDiffDecorations(ed, lineKinds ?? null);
    },
    [editorRef, applyDiffDecorations, lineKinds]
  );

  // 内容变化时更新 diff 装饰
  useEffect(() => {
    const ed = editorInstanceRef.current;
    if (ed) applyDiffDecorations(ed, lineKinds ?? null);
  }, [lineKinds, applyDiffDecorations]);

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
        glyphMargin: true,
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
