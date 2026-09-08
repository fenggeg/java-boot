// DiffView：Git 差异对比面板（P0），与编辑器并排共存。
// original = HEAD 版本内容（git cat-file 取回），modified = 当前缓冲区内容（实时跟随输入）。
// 滚动同步：monaco 0.52 内置同步在 @monaco-editor/react 反复 setModel/setValue 的集成下
// 会失效（original prop 变化时库调用 original.setValue() 重置滚动），且纯像素级同步
// （直接复制 scrollTop）在两侧行数不同时位置必然错位。这里在 onMount 里按「行号映射」
// 做双向同步：source 顶部可见行 → 经 diff 的 ILineChange 映射到对端行号 →
// getTopForLineNumber 换算目标 scrollTop。带回环守卫 + 位置守卫 + diff 就绪守卫，
// native 生效时天然 no-op。

import { useEffect, useRef, useState } from "react";
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
  /** 主编辑器实例（用于与 Diff modified 侧双向同步滚动） */
  mainEditor?: editor.IStandaloneCodeEditor | null;
}

export default function DiffView({
  repoRoot,
  filePath,
  modified,
  readonly,
  mainEditor,
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
  const diffReadyRef = useRef(false);

  const handleDiffMount = (diffEditor: editor.IDiffEditor) => {
    const originalEditor = diffEditor.getOriginalEditor();
    const modifiedEditor = diffEditor.getModifiedEditor();

    // 隐藏 modified 侧行号：主编辑器已显示行号，并排时避免出现重复两列行号
    modifiedEditor.updateOptions({ lineNumbers: "off" });

    // 清理上一次的 disposable（组件复用场景）
    disposablesRef.current.forEach((d) => d.dispose());
    disposablesRef.current = [];
    diffReadyRef.current = false;

    // diff 计算完成标记：onDidUpdateDiff 在 diff 重新计算完成后触发，
    // 未就绪前 getLineChanges() 返回 null/空数组，此时映射会错位
    disposablesRef.current.push(
      diffEditor.onDidUpdateDiff(() => {
        diffReadyRef.current = true;
      })
    );

    // 回环守卫：分为内外两层，互不干扰。
    // internalSyncing：diff 内部 original↔modified 双向同步
    // externalSyncing：主编辑器 ↔ diff modified 侧双向同步
    // 之前共用单个 syncing 导致：用户滚动 diff 时 attach(original→modified)
    // 设置 syncing=true，随后 modifiedEditor.onDidScrollChange 上注册的
    // diffToMain 监听器看到 syncing=true 直接 return，造成 diff→主编辑器
    // 单向失灵。
    let internalSyncing = false;
    let externalSyncing = false;

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
        if (internalSyncing) return;
        // diff 就绪守卫：未就绪时 getLineChanges() 返回 null/空数组，
        // 两侧行号含义不同（HEAD 行号 vs 当前行号），直接映射会错位
        if (!diffReadyRef.current) return;
        const changes = diffEditor.getLineChanges() ?? [];
        if (changes.length === 0) return;
        const mapped = mapLine(topLineAt(source, e.scrollTop), forward, changes);
        const targetTop = target.getTopForLineNumber(mapped);
        // 位置守卫：目标已在相近位置（native 同步或已同步）→ no-op，防回环
        if (Math.abs(target.getScrollTop() - targetTop) < 2) return;
        internalSyncing = true;
        try {
          target.setScrollPosition({
            scrollTop: targetTop,
            scrollLeft: e.scrollLeft,
          });
        } finally {
          internalSyncing = false;
        }
      });
      disposablesRef.current.push(disposable);
    };

    attach(originalEditor, modifiedEditor, true);
    attach(modifiedEditor, originalEditor, false);

    // 主编辑器 ↔ Diff modified 侧同步滚动：
    // 两者内容相同（当前缓冲区），行号一一对应，直接按行号换算 scrollTop。
    // 回环守卫使用独立 externalSyncing；位置守卫避免 native 联动回环。
    if (mainEditor) {
      const mainToDiff = mainEditor.onDidScrollChange((e) => {
        if (externalSyncing) return;
        const topLine = topLineAt(mainEditor, e.scrollTop);
        const targetTop = modifiedEditor.getTopForLineNumber(topLine);
        if (Math.abs(modifiedEditor.getScrollTop() - targetTop) < 2) return;
        externalSyncing = true;
        try {
          modifiedEditor.setScrollPosition({
            scrollTop: targetTop,
            scrollLeft: e.scrollLeft,
          });
        } finally {
          externalSyncing = false;
        }
      });
      const diffToMain = modifiedEditor.onDidScrollChange((e) => {
        if (externalSyncing) return;
        const topLine = topLineAt(modifiedEditor, e.scrollTop);
        const targetTop = mainEditor.getTopForLineNumber(topLine);
        if (Math.abs(mainEditor.getScrollTop() - targetTop) < 2) return;
        externalSyncing = true;
        try {
          mainEditor.setScrollPosition({
            scrollTop: targetTop,
            scrollLeft: e.scrollLeft,
          });
        } finally {
          externalSyncing = false;
        }
      });
      disposablesRef.current.push(mainToDiff, diffToMain);
    }
  };

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
