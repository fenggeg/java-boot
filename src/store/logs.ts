import type {StateCreator} from "zustand";
import type {LogLine} from "../types";
import type {LogsSlice, Store} from "./types";

// ================================================================
// 日志批量节流：高频日志先累积到 pending 队列，定时合并刷入 store，
// 避免每条日志都触发 setState 重渲染导致白屏。
// ================================================================

/** 按 serviceId 分组的待刷入日志队列 */
const pendingLogs: Record<string, LogLine[]> = {};
/** 节流定时器句柄 */
let flushTimer: ReturnType<typeof setTimeout> | null = null;
/** 节流间隔（ms）：在窗口内到达的所有日志合并为一次 store 更新 */
const FLUSH_INTERVAL = 50;

// HMR 清理：Vite 热更新时清除旧定时器，避免旧 flushTimer 触发新模块的 store
if (import.meta.hot) {
  import.meta.hot.dispose(() => {
    if (flushTimer) {
      clearTimeout(flushTimer);
      flushTimer = null;
    }
    for (const k of Object.keys(pendingLogs)) {
      delete pendingLogs[k];
    }
  });
}

/** 供 services 切片在删除服务时清理 pending 队列 */
export function dropPendingLogs(serviceId: string) {
  delete pendingLogs[serviceId];
}

export const createLogsSlice: StateCreator<
  Store,
  [],
  [],
  LogsSlice
> = (set, get) => {
  // 批量 flush：把 pending 队列里的日志合并写入 store
  const flushLogs = () => {
    flushTimer = null;
    const pendingIds = Object.keys(pendingLogs);
    if (pendingIds.length === 0) return;
    const state = get();
    const maxLines = state.config.log_buffer_lines || 10000;
    // 收集当前有效的服务 ID 集合，用于过滤已删除服务的残留事件
    const validServiceIds = new Set(state.services.map((s) => s.id));
    const nextLogs = { ...state.logs };
    for (const sid of pendingIds) {
      // 跳过已删除服务的日志，避免"复活"
      if (!validServiceIds.has(sid)) {
        delete pendingLogs[sid];
        continue;
      }
      const batch = pendingLogs[sid];
      if (!batch || batch.length === 0) continue;
      delete pendingLogs[sid];
      const existing = nextLogs[sid] ?? { lines: [], hasUnread: false };
      // 构造新数组而非原地 push，避免 zustand 引用相等判断失效
      const lines = [...existing.lines, ...batch];
      // 超限截断：同样构造新数组
      const trimmed = lines.length > maxLines
        ? lines.slice(lines.length - maxLines)
        : lines;
      const isSelected = state.selectedServiceId === sid;
      const isPaused = state.paused[sid];
      // 暂停的服务：日志已写入 lines 数组（缓存），但不生成新 LogBuffer 引用，
      // 避免触发订阅重渲染；恢复时会手动递增 logFlushTick 强制刷新。
      if (!isPaused) {
        // 选中服务始终未读=false；非选中：仅在原本未读或本次有新行时置 true
        nextLogs[sid] = {
          lines: trimmed,
          hasUnread: isSelected ? false : true,
        };
      } else {
        nextLogs[sid] = { lines: trimmed, hasUnread: existing.hasUnread };
      }
    }
    // 仅在确实有服务日志变更时 set，避免空 flush 触发重渲染
    set({ logs: nextLogs });
  };

  // 调度一次 flush（若已有定时器则复用，实现窗口内合并）
  const scheduleFlush = () => {
    if (flushTimer !== null) return;
    flushTimer = setTimeout(flushLogs, FLUSH_INTERVAL);
  };

  return {
    logs: {},
    paused: {},
    logFlushTick: 0,

    appendLog: (log) => {
      // 跳过已删除服务的日志，避免"复活"
      if (!get().services.some((s) => s.id === log.service_id)) return;
      // 不直接 set，而是推入 pending 队列，由节流定时器批量 flush。
      // 这样高频日志下 setState 频率从"每条一次"降到"每 FLUSH_INTERVAL 一次"。
      const queue = pendingLogs[log.service_id];
      if (queue) {
        queue.push(log);
      } else {
        pendingLogs[log.service_id] = [log];
      }
      scheduleFlush();
    },

    clearLog: (serviceId) => {
      // 清空 pending 队列中该服务的待刷入日志，避免清空后又被 flush 重新写入
      delete pendingLogs[serviceId];
      set((state) => ({
        logs: {
          ...state.logs,
          [serviceId]: { lines: [], hasUnread: false },
        },
      }));
    },

    markRead: (serviceId) => {
      set((state) => {
        const buf = state.logs[serviceId];
        if (!buf || !buf.hasUnread) return state;
        return {
          logs: {
            ...state.logs,
            [serviceId]: { ...buf, hasUnread: false },
          },
        };
      });
    },

    togglePause: (serviceId) => {
      set((state) => {
        const nextPaused = !state.paused[serviceId];
        // 恢复显示时：递增 logFlushTick 并为该服务生成新 LogBuffer 引用，
        // 强制订阅该服务的组件重新渲染，展示暂停期间缓存的日志。
        if (!nextPaused) {
          const buf = state.logs[serviceId];
          const isSelected = state.selectedServiceId === serviceId;
          return {
            paused: { ...state.paused, [serviceId]: false },
            logFlushTick: state.logFlushTick + 1,
            logs: buf
              ? {
                  ...state.logs,
                  [serviceId]: { lines: buf.lines, hasUnread: !isSelected },
                }
              : state.logs,
          };
        }
        return { paused: { ...state.paused, [serviceId]: true } };
      });
    },
  };
};
