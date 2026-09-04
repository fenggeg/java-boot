// useGitGutter：Monaco 编辑器的 Git 集成 Hook。
// - P0：gutter 变更标记（绿=新增 / 黄=修改 / 红=删除），含 minimap 与 overview ruler 着色；
// - P2：hover 行号显示 blame（提交摘要 / 作者 / 时间）；
// - P2：点击删除标记用 view zone 内联展示被删除的原始代码。
//
// 关键设计：
// - 用 editor.createDecorationsCollection() 管理（monaco >= 0.34，当前 0.52）；
// - 打字时不重算 diff：decoration 锚点设置 stickiness 自动跟随编辑，
//   仅在文件加载完成、收到 git://changed 时刷新；
// - 纯删除标记位置 newStart + 1，且 clamp 到 model.getLineCount()。
//
// 说明：所有可变状态（decorations / blame 缓存 / view zone / tooltip）都是本 effect
// 的局部变量——effect 依赖 [editor, repoRoot, filePath, enabled, monaco]，任一变化
// 都会重建整个 effect，因此不存在跨 effect 的陈旧闭包，也无需用 ref 传递。

import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type { editor } from "monaco-editor";
import type * as monacoNs from "monaco-editor";
import { GIT_CHANGED_EVENT, gitBlame, gitFileAtHead, gitFileDiff } from "./api";
import type { BlameLine, FileDiff, Hunk } from "./api";

const GIT_COLORS = {
  added: "#2da44e",
  modified: "#bf8700",
  deleted: "#cf222e",
} as const;

export interface UseGitGutterResult {
  diff: FileDiff | null;
  loading: boolean;
}

/**
 * @param editor   Monaco 编辑器实例（onEditorReady 回调获得；null 时跳过）
 * @param monaco   本地打包的 monaco 命名空间（monaco-setup 导出）
 * @param repoRoot 解析后的真实仓库根（git 可用且为仓库时才有值）
 * @param filePath 当前文件路径（项目相对，正斜杠；空串时跳过）
 * @param enabled  是否启用 git UI（git 已安装且为仓库）
 */
export function useGitGutter(
  editor: editor.IStandaloneCodeEditor | null,
  monaco: typeof monacoNs,
  repoRoot: string | null,
  filePath: string,
  enabled: boolean
): UseGitGutterResult {
  const [diff, setDiff] = useState<FileDiff | null>(null);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!editor || !enabled || !repoRoot || !filePath) return;
    const model = editor.getModel();
    if (!model) return;

    let cancelled = false;
    const coll = editor.createDecorationsCollection();
    // 纯删除标记行 → hunk（view zone 用）
    const deletedHunks = new Map<number, Hunk>();
    // blame 缓存（finalLine → BlameLine；git://changed 后失效重新加载）
    const blameCache = new Map<number, BlameLine>();
    // view zone：key = `${filePath}#${line}` → zone id
    const zoneIdByKey = new Map<string, string>();
    // blame 悬浮提示 DOM
    let tooltip: HTMLDivElement | null = null;

    /** 应用 diff 到 gutter 标记（打字不重算，锚点 stickiness 自动跟随） */
    const applyDiff = (d: FileDiff) => {
      const lineCount = model.getLineCount();
      const decorations: editor.IModelDeltaDecoration[] = [];
      deletedHunks.clear();
      for (const h of d.hunks) {
        if (h.newLines === 0) {
          // 纯删除：标记画在 newStart+1（间隙后第一行），clamp 到 lineCount
          const line = Math.min(h.newStart + 1, lineCount);
          if (line < 1 || line > lineCount) continue;
          deletedHunks.set(line, h);
          decorations.push({
            range: new monaco.Range(line, 1, line, 1),
            options: {
              isWholeLine: true,
              linesDecorationsClassName: "jb-gutter-deleted",
              overviewRuler: {
                color: GIT_COLORS.deleted,
                position: monaco.editor.OverviewRulerLane.Left,
              },
              minimap: {
                color: GIT_COLORS.deleted,
                position: monaco.editor.MinimapPosition.Gutter,
              },
              stickiness:
                monaco.editor.TrackedRangeStickiness.NeverGrowsWhenTypingAtEdges,
            },
          });
        } else {
          const start = h.newStart;
          const end = Math.min(h.newStart + h.newLines - 1, lineCount);
          if (start < 1 || end < start) continue;
          const added = h.oldLines === 0;
          const color = added ? GIT_COLORS.added : GIT_COLORS.modified;
          decorations.push({
            range: new monaco.Range(start, 1, end, 1),
            options: {
              isWholeLine: true,
              linesDecorationsClassName: added
                ? "jb-gutter-added"
                : "jb-gutter-modified",
              overviewRuler: {
                color,
                position: monaco.editor.OverviewRulerLane.Left,
              },
              minimap: {
                color,
                position: monaco.editor.MinimapPosition.Gutter,
              },
              stickiness:
                monaco.editor.TrackedRangeStickiness.NeverGrowsWhenTypingAtEdges,
            },
          });
        }
      }
      coll.set(decorations);
    };

    const refresh = async () => {
      if (cancelled) return;
      setLoading(true);
      try {
        const d = await gitFileDiff(repoRoot, filePath);
        if (cancelled) return;
        setDiff(d);
        applyDiff(d);
      } catch {
        if (!cancelled) {
          coll.clear();
          setDiff(null);
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
      // git 状态变化后失效 blame 缓存，下次 hover 重新加载
      blameCache.clear();
    };

    void refresh();

    // 后端 git://changed → 刷新 diff
    let unlisten: (() => void) | undefined;
    listen<null>(GIT_CHANGED_EVENT, () => void refresh()).then((f) => {
      if (cancelled) f();
      else unlisten = f;
    });

    // ---------------- P2：删除标记 view zone ----------------
    const buildZoneDom = (lines: string[], onClose: () => void) => {
      const wrap = document.createElement("div");
      wrap.className = "jb-deleted-zone";
      const header = document.createElement("div");
      header.className = "jb-deleted-zone-head";
      const label = document.createElement("span");
      label.textContent = `已删除 ${lines.length} 行`;
      const close = document.createElement("button");
      close.type = "button";
      close.className = "jb-deleted-zone-close";
      close.textContent = "收起";
      close.onclick = onClose;
      header.appendChild(label);
      header.appendChild(close);
      wrap.appendChild(header);
      const pre = document.createElement("pre");
      pre.className = "jb-deleted-zone-body";
      pre.textContent = lines.join("\n");
      wrap.appendChild(pre);
      return wrap;
    };

    const removeZone = (key: string) => {
      const id = zoneIdByKey.get(key);
      if (id) {
        editor.changeViewZones((accessor) => accessor.removeZone(id));
        zoneIdByKey.delete(key);
      }
    };

    const toggleViewZone = async (line: number, h: Hunk) => {
      const key = `${filePath}#${line}`;
      if (zoneIdByKey.has(key)) {
        removeZone(key);
        return;
      }
      const head = await gitFileAtHead(repoRoot, filePath);
      if (head == null) return;
      const all = head.split(/\r?\n/);
      const start = Math.max(0, h.oldStart - 1);
      const slice = all.slice(start, start + h.oldLines);
      if (slice.length === 0) return;
      editor.changeViewZones((accessor) => {
        const id = accessor.addZone({
          afterLineNumber: line,
          heightInLines: slice.length + 1,
          domNode: buildZoneDom(slice, () => removeZone(key)),
        });
        zoneIdByKey.set(key, id);
      });
    };

    // ---------------- P2：blame hover 悬浮 ----------------
    const ensureBlame = async () => {
      if (blameCache.size > 0) return;
      try {
        const lines = await gitBlame(repoRoot, filePath);
        blameCache.clear();
        for (const l of lines) blameCache.set(l.finalLine, l);
      } catch {
        /* 忽略：无历史/二进制等场景 hover 无数据即可 */
      }
    };

    const showTooltip = (bl: BlameLine, clientX: number, clientY: number) => {
      if (!tooltip) {
        tooltip = document.createElement("div");
        tooltip.className = "jb-blame-tooltip";
        document.body.appendChild(tooltip);
      }
      tooltip.innerHTML = "";
      const mk = (cls: string, text: string) => {
        const el = document.createElement("div");
        el.className = cls;
        el.textContent = text;
        tooltip!.appendChild(el);
      };
      mk("jb-blame-author", bl.author || "未知作者");
      mk("jb-blame-summary", bl.summary || "");
      mk(
        "jb-blame-meta",
        `${new Date(bl.time * 1000).toLocaleString()}  ·  ${bl.sha.slice(0, 7)}`
      );
      tooltip.style.display = "block";
      tooltip.style.left = `${Math.min(clientX + 14, window.innerWidth - 300)}px`;
      tooltip.style.top = `${Math.min(clientY + 14, window.innerHeight - 100)}px`;
    };

    const hideTooltip = () => {
      if (tooltip) tooltip.style.display = "none";
    };

    const onMouseMove = (e: editor.IEditorMouseEvent) => {
      const t = e.target;
      const isGutter =
        t.type === monaco.editor.MouseTargetType.GUTTER_LINE_NUMBERS ||
        t.type === monaco.editor.MouseTargetType.GUTTER_LINE_DECORATIONS;
      if (!isGutter || t.position == null) {
        hideTooltip();
        return;
      }
      const line = t.position.lineNumber;
      const bl = blameCache.get(line);
      if (!bl) {
        // 未加载过 blame → 懒加载一次
        if (blameCache.size === 0) void ensureBlame();
        hideTooltip();
        return;
      }
      showTooltip(bl, e.event.browserEvent.clientX, e.event.browserEvent.clientY);
    };

    const onMouseDown = (e: editor.IEditorMouseEvent) => {
      const t = e.target;
      const isGutter =
        t.type === monaco.editor.MouseTargetType.GUTTER_LINE_DECORATIONS ||
        t.type === monaco.editor.MouseTargetType.GUTTER_LINE_NUMBERS;
      if (!isGutter || t.position == null) return;
      const line = t.position.lineNumber;
      const h = deletedHunks.get(line);
      if (!h) return;
      void toggleViewZone(line, h);
    };

    const mouseMoveDisposable = editor.onMouseMove(onMouseMove);
    const mouseDownDisposable = editor.onMouseDown(onMouseDown);

    // 清理：集合、事件监听、view zone、tooltip
    return () => {
      cancelled = true;
      unlisten?.();
      mouseMoveDisposable.dispose();
      mouseDownDisposable.dispose();
      for (const key of [...zoneIdByKey.keys()]) removeZone(key);
      if (tooltip) {
        tooltip.remove();
        tooltip = null;
      }
      coll.clear();
      setDiff(null);
      setLoading(false);
    };
  }, [editor, repoRoot, filePath, enabled, monaco]);

  return { diff, loading };
}
