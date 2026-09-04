// DiffView：Git 差异对比面板（P0），与编辑器并排共存。
// original = HEAD 版本内容（git cat-file 取回），modified = 当前缓冲区内容（实时跟随输入）。
// 滚动同步：monaco 0.52 内置同步在 @monaco-editor/react 反复 setModel 的集成下可能失效，
// 且纯像素级同步（直接复制 scrollTop）在两侧行数不同时位置必然错位。这里在 onMount 里
// 按「行号映射」做双向同步：source 顶部可见行 → 经 diff 的 ILineChange 映射到对端行号 →
// getTopForLineNumber 换算目标 scrollTop。带回环守卫 + 位置守卫，native 生效时天然 no-op。

import { useCallback, useEffect, useRef, useState } from "react";
import { DiffEditor } from "@monaco-editor/react";
import type { editor, IDisposable } from "monaco-editor";
import { gitFileAtHead } from "./api";
import { getMonacoLang } from "../../languages";
import { getMonacoTheme } from "../../monaco-setup";

export interface DiffViewProps {
  /** 真实仓库根；null = 非仓库或 git 未安装（由父组件决定是否渲染） */
  repoRoot: string | null;
  /** 当前文件路径（项目相对，正斜杠） */
  filePath: string;
  /** 当前缓冲区内容（实时） */
  modified: string;
  /** 是否只读文件（Diff 只读展示） */
  readonly: boolean;
}

export default function DiffView({
  repoRoot,
  filePath,
  modified,
  readonly,
}: DiffViewProps) {
  const [original, setOriginal] = useState<string | null>(null);
  const [state, setState] = useState<
    "loading" | "ready" | "unavailable" | "no-repo"
  >("loading");

  useEffect(() => {
    let cancelled = false;
    setState("loading");
    setOriginal(null);
    if (!repoRoot) {
      setState("no-repo");
      return;
    }
    gitFileAtHead(repoRoot, filePath)
      .then((head) => {
        if (cancelled) return;
        if (head == null) {
          setState("unavailable");
        } else {
          setOriginal(head);
          setState("ready");
        }
      })
      .catch(() => {
        if (!cancelled) {
          setOriginal(null);
          setState("unavailable");
        }
      });
    return () => {
      cancelled = true;
    };
  }, [repoRoot, filePath]);

  /** 双向手动滚动同步：按 diff 行号映射（像素级复制在两侧行数不同时必然错位） */
  const disposablesRef = useRef<IDisposable[]>([]);
  const handleDiffMount = useCallback((diffEditor: editor.IDiffEditor) => {
    const originalEditor = diffEditor.getOriginalEditor();
    const modifiedEditor = diffEditor.getModifiedEditor();

    // 清理上一次的 disposable（组件复用场景）
    disposablesRef.current.forEach((d) => d.dispose());
    disposablesRef.current = [];

    // 回环守卫：手动设置对端滚动位置时不再反向触发同步
    let syncing = false;

    // 从 scrollTop 求顶部可见行号（二分查找）
    const topLineAt = (ed: editor.ICodeEditor, scrollTop: number): number => {
      const model = ed.getModel();
      if (!model) return 1;
      const lineCount = model.getLineCount();
      let lo = 1;
      let hi = lineCount;
      while (lo < hi) {
        const mid = Math.floor((lo + hi + 1) / 2);
        if (ed.getTopForLineNumber(mid) <= scrollTop) {
          lo = mid;
        } else {
          hi = mid - 1;
        }
      }
      return lo;
    };

    // 原行号 <-> 改后行号 映射。
    // ILineChange 空区间语义：originalStart>originalEnd → 纯新增（原侧无行）；
    // modifiedStart>modifiedEnd → 纯删除（改后无行）。changes 按起始行号升序。
    const mapLine = (
      line: number,
      forward: boolean,
      changes: editor.ILineChange[]
    ): number => {
      if (changes.length === 0) return line;
      let offset = 0;
      for (const c of changes) {
        if (forward) {
          const oStart = c.originalStartLineNumber;
          const oEnd = c.originalEndLineNumber;
          if (line < oStart) return line + offset;
          if (line <= oEnd) {
            // 落在变更块内：有改后行则块内对齐；纯删除映射到删除后的下一行
            return c.modifiedStartLineNumber <= c.modifiedEndLineNumber
              ? c.modifiedStartLineNumber + (line - oStart)
              : c.modifiedStartLineNumber;
          }
          offset = c.modifiedEndLineNumber - oEnd;
        } else {
          const mStart = c.modifiedStartLineNumber;
          const mEnd = c.modifiedEndLineNumber;
          if (line < mStart) return line + offset;
          if (line <= mEnd) {
            // 落在变更块内：有原行则块内对齐；纯新增锚定到插入位置前的原行
            return c.originalStartLineNumber <= c.originalEndLineNumber
              ? c.originalStartLineNumber + (line - mStart)
              : c.originalStartLineNumber;
          }
          offset = c.originalEndLineNumber - mEnd;
        }
      }
      return line + offset;
    };

    const attach = (
      source: editor.ICodeEditor,
      target: editor.ICodeEditor,
      forward: boolean
    ) => {
      const disposable = source.onDidScrollChange((e) => {
        if (syncing) return;
        const changes = diffEditor.getLineChanges() ?? [];
        const mapped = mapLine(topLineAt(source, e.scrollTop), forward, changes);
        const targetTop = target.getTopForLineNumber(mapped);
        // 位置守卫：目标已在相近位置（native 同步或已同步）→ no-op，防回环
        if (Math.abs(target.getScrollTop() - targetTop) < 2) return;
        syncing = true;
        try {
          target.setScrollPosition({
            scrollTop: targetTop,
            scrollLeft: e.scrollLeft,
          });
        } finally {
          syncing = false;
        }
      });
      disposablesRef.current.push(disposable);
    };

    attach(originalEditor, modifiedEditor, true);
    attach(modifiedEditor, originalEditor, false);
  }, []);

  // 组件卸载时清理所有 disposable，避免内存泄漏
  useEffect(() => {
    return () => {
      disposablesRef.current.forEach((d) => d.dispose());
      disposablesRef.current = [];
    };
  }, []);

  return (
    <div className="git-diff-panel">
      <div className="git-diff-head">
        <span className="git-diff-head-title">差异对比</span>
        <span className="git-diff-head-sub">HEAD ↔ 当前</span>
      </div>
      {state === "loading" ? (
        <div className="git-diff-hint">加载 HEAD 版本…</div>
      ) : state === "no-repo" ? (
        <div className="git-diff-hint">当前项目不是 Git 仓库</div>
      ) : state === "unavailable" || original == null ? (
        <div className="git-diff-hint">
          该文件尚未提交（不在 HEAD 中），暂无历史版本可对比
        </div>
      ) : (
        <DiffEditor
          original={original}
          modified={modified}
          language={getMonacoLang(filePath)}
          theme={getMonacoTheme()}
          height="100%"
          loading={<div className="git-diff-hint">加载编辑器…</div>}
          onMount={handleDiffMount}
          options={{
            readOnly: readonly,
            fontSize: 13,
            fontFamily: "var(--font-mono)",
            lineHeight: 22,
            lineNumbersMinChars: 4,
            renderSideBySide: true,
            minimap: { enabled: false },
            wordWrap: "off",
            scrollBeyondLastLine: false,
            automaticLayout: true,
            scrollbar: {
              verticalScrollbarSize: 10,
              horizontalScrollbarSize: 10,
              useShadows: false,
            },
            padding: { top: 12, bottom: 12 },
            renderLineHighlight: "line",
            stickyScroll: { enabled: true },
            fixedOverflowWidgets: true,
            overviewRulerBorder: false,
          }}
        />
      )}
    </div>
  );
}
