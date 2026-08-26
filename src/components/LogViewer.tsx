import {useCallback, useEffect, useMemo, useRef, useState} from "react";
import {App, Dropdown, Input, Segmented, Tooltip} from "antd";
import {ArrowDown, ChevronLeft, Clear, Pause, Play2, Search, Terminal} from "./Icons";
import {useStore} from "../store";
import type {LogLine} from "../types";

interface Props {
  serviceId: string | null;
}

type LogLevel = "all" | "info" | "warn" | "error";

function classifyLine(line: string): "info" | "warn" | "error" {
  const lower = line.toLowerCase();
  if (/\b(error|exception|fail(ed)?|fatal)\b/.test(lower)) return "error";
  if (/\b(warn(ing)?)\b/.test(lower)) return "warn";
  return "info";
}

function matchLevel(line: string, level: LogLevel): boolean {
  if (level === "all") return true;
  return classifyLine(line) === level;
}

function sourceClass(source: string): string {
  if (source.includes("mvn")) return "mvn";
  if (source.includes("git")) return "git";
  return "app";
}

const LINE_HEIGHT = 19;
const OVERSCAN = 30;

/** 渲染单行日志时，高亮搜索匹配片段 */
function renderLine(text: string, regex: RegExp | null): React.ReactNode {
  if (!regex) return text;
  const parts: React.ReactNode[] = [];
  let last = 0;
  let m: RegExpExecArray | null;
  // 重置 regex lastIndex（全局 g flag）
  regex.lastIndex = 0;
  let i = 0;
  while ((m = regex.exec(text)) !== null) {
    if (m.index > last) parts.push(text.slice(last, m.index));
    parts.push(
      <mark className="log-highlight" key={i++}>
        {m[0]}
      </mark>
    );
    last = m.index + m[0].length;
    // 防止零宽匹配死循环
    if (m[0] === "") regex.lastIndex++;
  }
  if (last < text.length) parts.push(text.slice(last));
  return parts;
}

export default function LogViewer({ serviceId }: Props) {
  const logs = useStore((s) => (serviceId ? s.logs[serviceId] : undefined));
  const clearLog = useStore((s) => s.clearLog);
  const isPaused = useStore((s) => (serviceId ? !!s.paused[serviceId] : false));
  const togglePause = useStore((s) => s.togglePause);
  // 订阅 logFlushTick：暂停恢复时 store 递增此值，强制 filtered 重算 + 组件刷新
  const logFlushTick = useStore((s) => s.logFlushTick);
  const [autoScroll, setAutoScroll] = useState(true);
  const [search, setSearch] = useState("");
  const [useRegex, setUseRegex] = useState(false);
  const [level, setLevel] = useState<LogLevel>("all");
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportH, setViewportH] = useState(600);
  // 当前高亮的搜索匹配索引（从 0 开始）
  const [matchIdx, setMatchIdx] = useState(0);
  // 日志右键菜单
  const [logContextMenu, setLogContextMenu] = useState<{
    x: number;
    y: number;
    selectedText: string;
  } | null>(null);
  const { message } = App.useApp();
  const containerRef = useRef<HTMLDivElement>(null);

  // 日志右键菜单处理
  const handleLogContextMenu = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    const selection = window.getSelection();
    const selectedText = selection?.toString() ?? "";
    setLogContextMenu({ x: e.clientX, y: e.clientY, selectedText });
  }, []);

  const closeLogContextMenu = useCallback(() => setLogContextMenu(null), []);

  // 编译搜索正则
  const searchRegex = useMemo<RegExp | null>(() => {
    if (!search.trim()) return null;
    if (useRegex) {
      try {
        return new RegExp(search, "gi");
      } catch {
        return null; // 正则无效时静默退化为无高亮
      }
    }
    // 转义正则特殊字符
    const escaped = search.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    return new RegExp(escaped, "gi");
  }, [search, useRegex]);

  const allLines = logs?.lines ?? [];
  const filtered = useMemo<LogLine[]>(() => {
    const searchLower = search.toLowerCase();
    void logFlushTick;
    return (logs?.lines ?? []).filter((l) => {
      if (!matchLevel(l.line, level)) return false;
      if (search) {
        if (useRegex && searchRegex) {
          if (!searchRegex.test(l.line)) return false;
          // reset lastIndex for subsequent highlight render
          searchRegex.lastIndex = 0;
        } else if (!useRegex && !l.line.toLowerCase().includes(searchLower)) {
          return false;
        }
      }
      return true;
    });
  }, [logs, level, search, useRegex, searchRegex, logFlushTick]);

  // 日志右键菜单处理（需在 filtered 定义之后，因为 copyAll 引用 filtered）
  const handleLogContextAction = useCallback((action: string) => {
    const sel = window.getSelection();
    switch (action) {
      case "copy":
        if (sel && sel.toString()) {
          navigator.clipboard.writeText(sel.toString()).then(() => {
            message.success("已复制");
          }).catch(() => {});
        }
        break;
      case "copyAll":
        navigator.clipboard.writeText(
          filtered.map((l) => l.line).join("\n")
        ).then(() => {
          message.success("已复制全部日志");
        }).catch(() => {});
        break;
      case "selectAll":
        if (containerRef.current) {
          const range = document.createRange();
          range.selectNodeContents(containerRef.current);
          sel?.removeAllRanges();
          sel?.addRange(range);
        }
        break;
      case "clear":
        clearLog(serviceId!);
        message.success("已清空日志");
        break;
      case "search":
        // 在搜索框中填充选中文本
        if (logContextMenu?.selectedText) {
          setSearch(logContextMenu.selectedText);
          setUseRegex(false);
        }
        break;
    }
    closeLogContextMenu();
  }, [filtered, serviceId, clearLog, message, logContextMenu, closeLogContextMenu]);

  // 计算匹配搜索关键词的行索引（在 filtered 数组中的位置）
  const matchIndices = useMemo<number[]>(() => {
    if (!search.trim() || !searchRegex) return [];
    void logFlushTick;
    const indices: number[] = [];
    for (let i = 0; i < filtered.length; i++) {
      const l = filtered[i]!;
      if (searchRegex.test(l.line)) {
        indices.push(i);
      }
      searchRegex.lastIndex = 0;
    }
    return indices;
  }, [filtered, search, searchRegex, logFlushTick]);

  const matchCount = matchIndices.length;

  // 搜索词变化时重置匹配索引
  useEffect(() => {
    setMatchIdx(0);
  }, [search, useRegex]);

  const handleScroll = useCallback(() => {
    const el = containerRef.current;
    if (!el) return;
    setScrollTop(el.scrollTop);
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    setAutoScroll(atBottom);
  }, []);

  // 跳转到第 idx 个匹配
  const jumpToMatch = useCallback(
    (idx: number) => {
      if (matchIndices.length === 0) return;
      const clamped = ((idx % matchIndices.length) + matchIndices.length) % matchIndices.length;
      const targetLine = matchIndices[clamped]!;
      const targetScrollTop = targetLine * LINE_HEIGHT;
      const el = containerRef.current;
      if (el) {
        el.scrollTop = targetScrollTop;
        setScrollTop(targetScrollTop);
      }
      setMatchIdx(clamped);
    },
    [matchIndices]
  );

  // 监听容器尺寸
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      setViewportH(el.clientHeight);
    });
    ro.observe(el);
    setViewportH(el.clientHeight);
    return () => ro.disconnect();
  }, []);

  // 自动滚动到底部（新日志到达时）
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    if (autoScroll) {
      el.scrollTop = el.scrollHeight;
      setScrollTop(el.scrollTop);
    }
  }, [filtered.length, autoScroll]);

  // 切换服务时重置状态，并主动跳到最底部
  // （不能依赖上面的滚动 effect：若新旧服务行数相同且 autoScroll 已为 true，
  //   依赖不变不会触发，会停留在上一个服务的滚动位置）
  useEffect(() => {
    setAutoScroll(true);
    setLevel("all");
    setSearch("");
    setUseRegex(false);
    setMatchIdx(0);
    // 双 rAF：等待新服务的虚拟列表按新数据完成布局后再滚动到底
    let raf2 = 0;
    const raf1 = requestAnimationFrame(() => {
      raf2 = requestAnimationFrame(() => {
        const el = containerRef.current;
        if (el) {
          el.scrollTop = el.scrollHeight;
          setScrollTop(el.scrollTop);
        }
      });
    });
    return () => {
      cancelAnimationFrame(raf1);
      cancelAnimationFrame(raf2);
    };
  }, [serviceId]);

  if (!serviceId) {
    return (
      <div className="log-empty">
        <div style={{ textAlign: "center", display: "flex", flexDirection: "column", alignItems: "center", gap: 12 }}>
          <span style={{ color: "var(--text-3)", display: "inline-flex" }}>
            <Terminal size={36} />
          </span>
          <div style={{ fontSize: 13, color: "var(--text-3)" }}>
            选择左侧服务查看日志
          </div>
        </div>
      </div>
    );
  }

  // 虚拟滚动计算
  const totalHeight = filtered.length * LINE_HEIGHT;
  const startIdx = Math.max(0, Math.floor(scrollTop / LINE_HEIGHT) - OVERSCAN);
  const endIdx = Math.min(
    filtered.length,
    Math.ceil((scrollTop + viewportH) / LINE_HEIGHT) + OVERSCAN
  );
  const renderLines = filtered.slice(startIdx, endIdx);
  const offsetY = startIdx * LINE_HEIGHT;

  return (
    <div className="log-panel">
      <div className="log-toolbar">
        <Segmented
          size="small"
          value={level}
          onChange={(v) => setLevel(v as LogLevel)}
          options={[
            { label: "全部", value: "all" },
            { label: "INFO", value: "info" },
            { label: "WARN", value: "warn" },
            { label: "ERROR", value: "error" },
          ]}
        />
        <Input
          size="small"
          allowClear
          prefix={<Search size={13} style={{ color: "var(--text-3)" }} />}
          placeholder={useRegex ? "正则搜索..." : "搜索日志..."}
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          onPressEnter={() => jumpToMatch(matchIdx + 1)}
          style={{ width: 200 }}
          suffix={
            <Tooltip title={useRegex ? "切换为普通搜索" : "切换为正则搜索"}>
              <button
                className={`icon-btn sm ${useRegex ? "accent" : ""}`}
                onClick={() => setUseRegex((v) => !v)}
                aria-label="切换正则模式"
                style={{ width: 20, height: 20, marginRight: -2, fontSize: 10, fontWeight: 700, fontFamily: "var(--font-mono)" }}
              >
                .*
              </button>
            </Tooltip>
          }
        />
        {matchCount > 0 && (
          <div style={{ display: "flex", alignItems: "center", gap: 2 }}>
            <Tooltip title="上一个匹配">
              <button
                className="icon-btn sm"
                onClick={() => jumpToMatch(matchIdx - 1)}
                aria-label="上一个匹配"
              >
                <ChevronLeft size={13} />
              </button>
            </Tooltip>
            <span className="toolbar-count" style={{ minWidth: 48, textAlign: "center" }}>
              {matchIdx + 1}/{matchCount}
            </span>
            <Tooltip title="下一个匹配">
              <button
                className="icon-btn sm"
                onClick={() => jumpToMatch(matchIdx + 1)}
                aria-label="下一个匹配"
                style={{ transform: "rotate(180deg)" }}
              >
                <ChevronLeft size={13} />
              </button>
            </Tooltip>
          </div>
        )}
        {search && matchCount === 0 && (
          <span className="toolbar-count" style={{ color: "var(--text-4)" }}>
            无匹配
          </span>
        )}
        {!search && (
          <span className="toolbar-count">
            {filtered.length} / {allLines.length} 行
          </span>
        )}
        <div style={{ marginLeft: "auto", display: "flex", gap: 4 }}>
          {!autoScroll && !isPaused && (
            <Tooltip title="滚动到底部">
              <button
                className="icon-btn sm accent"
                onClick={() => {
                  setAutoScroll(true);
                  if (containerRef.current)
                    containerRef.current.scrollTop =
                      containerRef.current.scrollHeight;
                }}
                aria-label="滚动到底部"
              >
                <ArrowDown size={13} />
              </button>
            </Tooltip>
          )}
          <Tooltip title={isPaused ? "继续打印" : "暂停打印"}>
            <button
              className={`icon-btn sm ${isPaused ? "accent" : ""}`}
              onClick={() => togglePause(serviceId)}
              aria-label={isPaused ? "继续打印" : "暂停打印"}
            >
              {isPaused ? <Play2 size={13} /> : <Pause size={13} />}
            </button>
          </Tooltip>
          <Tooltip title="清空日志">
            <button
              className="icon-btn sm"
              onClick={() => clearLog(serviceId)}
              aria-label="清空日志"
            >
              <Clear size={13} />
            </button>
          </Tooltip>
        </div>
      </div>
      <div
        className="log-viewer"
        ref={containerRef}
        onScroll={handleScroll}
        onContextMenu={handleLogContextMenu}
        style={{ position: "relative" }}
      >
        {isPaused && (
          <div
            style={{
              position: "absolute",
              top: 8,
              left: "50%",
              transform: "translateX(-50%)",
              zIndex: 10,
              background: "rgba(255, 159, 0, 0.15)",
              border: "1px solid rgba(255, 159, 0, 0.5)",
              color: "#ff9500",
              fontSize: 12,
              padding: "3px 12px",
              borderRadius: 12,
              pointerEvents: "none",
              fontFamily: "var(--font-sans)",
            }}
          >
            日志已暂停，继续打印后将显示暂停期间缓存的日志
          </div>
        )}
        {renderLines.length === 0 ? (
          <div
            style={{
              color: "var(--text-4)",
              textAlign: "center",
              padding: 40,
              fontFamily: "var(--font-sans)",
              fontSize: 13,
              position: "relative",
              zIndex: 2,
            }}
          >
            暂无日志
          </div>
        ) : (
          <div style={{ height: totalHeight, position: "relative" }}>
            <div style={{ transform: `translateY(${offsetY}px)` }}>
              {renderLines.map((l, i) => {
                const lv = classifyLine(l.line);
                const absIdx = startIdx + i;
                // 当前选中的匹配行高亮
                const isCurrentMatch =
                  matchCount > 0 && matchIndices[matchIdx] === absIdx;
                return (
                  <div
                    key={i}
                    className={`log-line ${sourceClass(l.source)} ${lv} ${isCurrentMatch ? "log-match-current" : ""}`}
                    style={{ height: LINE_HEIGHT, minHeight: LINE_HEIGHT }}
                  >
                    <span className="log-time">{l.ts.slice(11, 19)}</span>
                    <span className="log-source">{l.source}</span>
                    {renderLine(l.line, searchRegex)}
                  </div>
                );
              })}
            </div>
          </div>
        )}
      </div>
      {/* 日志右键菜单 */}
      {logContextMenu && (
        <Dropdown
          open={true}
          trigger={["contextMenu"]}
          onOpenChange={(open) => { if (!open) closeLogContextMenu(); }}
          menu={{
            items: [
              {
                key: "copy",
                label: "复制选中",
                disabled: !logContextMenu.selectedText,
                onClick: () => handleLogContextAction("copy"),
              },
              {
                key: "copyAll",
                label: "复制全部日志",
                onClick: () => handleLogContextAction("copyAll"),
              },
              { type: "divider" as const },
              {
                key: "search",
                label: "搜索选中内容",
                disabled: !logContextMenu.selectedText,
                onClick: () => handleLogContextAction("search"),
              },
              { type: "divider" as const },
              {
                key: "clear",
                label: "清空日志",
                danger: true,
                onClick: () => handleLogContextAction("clear"),
              },
            ],
          }}
        >
          <div
            style={{
              position: "fixed",
              left: logContextMenu.x,
              top: logContextMenu.y,
              // 锚点必须非 0 尺寸：0x0 的 fixed 元素会被 rc-trigger 判定为
              // 不可见（offsetParent 为 null 且宽高为 0），导致弹层永不对齐、菜单无法显示
              width: 1,
              height: 1,
              opacity: 0,
              pointerEvents: "none",
            }}
          />
        </Dropdown>
      )}
    </div>
  );
}
