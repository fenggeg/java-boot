import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Spin } from "antd";
import * as api from "../api";
import { dirOf } from "./filePanelUtils";
import type { QuickHit } from "./filePanelUtils";
import { FileTypeIcon } from "./FileTreeRow";

interface Props {
  projectId: string;
  openFile: (path: string) => void;
  /** 面板是否可见（不可见时不响应 Ctrl+P） */
  visible: boolean;
}

/**
 * 快速打开（Ctrl+P）：项目级文件名 / 路径过滤跳转。
 * 自包含组件：管理自己的状态（弹层开关、查询、选中、文件列表）。
 */
export default function QuickOpenPanel({ projectId, openFile, visible }: Props) {
  const [quickOpen, setQuickOpen] = useState(false);
  const [quickQuery, setQuickQuery] = useState("");
  const [quickIdx, setQuickIdx] = useState(0);
  // 全量扁平文件列表；每次打开弹层都后台重拉，杜绝树操作后的陈旧索引
  const [flatFiles, setFlatFiles] = useState<api.FlatFile[] | null>(null);
  const quickListRef = useRef<HTMLDivElement>(null);

  /** 拉取全量文件列表；失败时保留上次结果（退化为旧数据可用） */
  const loadFlatFiles = useCallback(async () => {
    try {
      const files = await api.walkFiles(projectId);
      setFlatFiles(files);
    } catch {
      /* 保留上一次列表 */
    }
  }, [projectId]);

  /**
   * 过滤与排序（小写不区分大小写）：文件名前缀 > 文件名包含（位置越靠前越优）
   * > 路径包含。空查询展示前 50 个当浏览列表。全量收集后排序取前 100——
   * 过滤本身就要扫全部条目，截断省不出成本，只换掉排序正确性。
   */
  const quickResults = useMemo<QuickHit[]>(() => {
    if (!flatFiles) return [];
    const q = quickQuery.trim().toLowerCase();
    if (!q) {
      return flatFiles.slice(0, 50).map((f) => ({
        path: f.path,
        name: f.name,
        dir: dirOf(f.path),
        score: 0,
      }));
    }
    const out: QuickHit[] = [];
    for (const f of flatFiles) {
      const pl = f.path.toLowerCase();
      const slash = pl.lastIndexOf("/");
      const nl = slash < 0 ? pl : pl.slice(slash + 1);
      let score: number | null;
      if (nl.startsWith(q)) {
        score = 0;
      } else {
        const ni = nl.indexOf(q);
        if (ni >= 0) {
          score = 1 + Math.min(ni, 999) / 1000;
        } else {
          const pi = pl.indexOf(q);
          score = pi >= 0 ? 10 + Math.min(pi, 999) / 1000 : null;
        }
      }
      if (score !== null) {
        out.push({path: f.path, name: f.name, dir: dirOf(f.path), score});
      }
    }
    out.sort(
      (a, b) =>
        a.score - b.score ||
        a.path.length - b.path.length ||
        a.path.localeCompare(b.path)
    );
    return out.slice(0, 100);
  }, [flatFiles, quickQuery]);

  // 结果集变化时收敛选中下标
  useEffect(() => {
    setQuickIdx((i) =>
      quickResults.length ? Math.min(i, quickResults.length - 1) : 0
    );
  }, [quickResults.length]);

  // 选中项滚动进可视区
  useEffect(() => {
    quickListRef.current
      ?.querySelector(".quick-open-item.sel")
      ?.scrollIntoView({block: "nearest"});
  }, [quickIdx]);

  const openQuickOpen = useCallback(() => {
    setQuickOpen(true);
    setQuickQuery("");
    setQuickIdx(0);
    void loadFlatFiles();
  }, [loadFlatFiles]);

  const closeQuickOpen = useCallback(() => {
    setQuickOpen(false);
    setQuickQuery("");
    setQuickIdx(0);
  }, []);

  /** 回车 / 点击：打开选中文件并收起弹层 */
  const acceptQuick = useCallback(() => {
    const hit = quickResults[quickIdx];
    if (!hit) return;
    void openFile(hit.path);
    closeQuickOpen();
  }, [quickResults, quickIdx, openFile, closeQuickOpen]);

  // Ctrl+P 全局唤起；Esc 在输入框内 stopPropagation 关闭，不外溢
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (
        (e.ctrlKey || e.metaKey) &&
        !e.altKey &&
        !e.shiftKey &&
        e.key.toLowerCase() === "p"
      ) {
        if (!visible) return;
        e.preventDefault();
        e.stopPropagation();
        openQuickOpen();
      } else if (e.key === "Escape" && quickOpen) {
        closeQuickOpen();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [openQuickOpen, quickOpen, closeQuickOpen, visible]);

  if (!quickOpen) return null;

  return (
    <>
      <div className="quick-open-mask" onClick={closeQuickOpen} />
      <div className="quick-open">
        <input
          autoFocus
          className="quick-open-input"
          value={quickQuery}
          onChange={(e) => {
            setQuickQuery(e.target.value);
            setQuickIdx(0);
          }}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") {
              e.preventDefault();
              setQuickIdx((i) =>
                quickResults.length ? (i + 1) % quickResults.length : 0
              );
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              setQuickIdx((i) =>
                quickResults.length
                  ? (i - 1 + quickResults.length) % quickResults.length
                  : 0
              );
            } else if (e.key === "Enter") {
              e.preventDefault();
              acceptQuick();
            } else if (e.key === "Escape") {
              e.stopPropagation();
              closeQuickOpen();
            }
          }}
          placeholder="输入文件名或路径过滤，回车打开…"
          spellCheck={false}
          autoComplete="off"
        />
        <div className="quick-open-list" ref={quickListRef}>
          {!flatFiles ? (
            <div className="quick-open-empty">
              <Spin size="small" />
            </div>
          ) : quickResults.length === 0 ? (
            <div className="quick-open-empty">无匹配文件</div>
          ) : (
            quickResults.map((h, i) => (
              <div
                key={h.path}
                className={`quick-open-item${i === quickIdx ? " sel" : ""}`}
                onMouseEnter={() => setQuickIdx(i)}
                onClick={acceptQuick}
              >
                <FileTypeIcon name={h.name} size={13} />
                <span className="quick-open-name">{h.name}</span>
                {h.dir && (
                  <span className="quick-open-dir" title={h.dir}>
                    {h.dir}
                  </span>
                )}
              </div>
            ))
          )}
        </div>
        <div className="quick-open-hint">
          <span>↑↓ 选择</span>
          <span>Enter 打开</span>
          <span>Esc 关闭</span>
        </div>
      </div>
    </>
  );
}
