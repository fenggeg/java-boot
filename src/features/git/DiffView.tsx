// DiffView：Git 差异对比面板（P0），与编辑器并排共存。
// original = HEAD 版本内容（git cat-file 取回），modified = 当前缓冲区内容（实时跟随输入）。

import { useEffect, useState } from "react";
import { DiffEditor } from "@monaco-editor/react";
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
