import {create} from "zustand";
import type {AppConfig, LogLine, Project, Service, ServiceRuntime,} from "./types";
import * as api from "./api";

interface LogBuffer {
  lines: LogLine[];
  hasUnread: boolean;
}

interface Store {
  projects: Project[];
  services: Service[];
  runtimes: Record<string, ServiceRuntime>;
  logs: Record<string, LogBuffer>;
  /** 按服务记录是否暂停日志显示（暂停期间日志仍缓存，但不触发 UI 更新） */
  paused: Record<string, boolean>;
  /** 日志批量 flush 的版本号，暂停恢复时递增以强制刷新订阅 */
  logFlushTick: number;
  config: AppConfig;
  selectedServiceId: string | null;
  /** 已打开的日志 Tab (IDE-like)，按打开顺序；只有在这里的 service 才会在日志区顶部显示 Tab */
  openedTabs: string[];
  gitAvailable: boolean;
  loading: boolean;

  // actions
  init: () => Promise<void>;
  refreshServices: () => Promise<void>;
  selectService: (id: string | null) => void;
  closeTab: (id: string) => void;
  setRuntime: (rt: ServiceRuntime) => void;
  appendLog: (log: LogLine) => void;
  clearLog: (serviceId: string) => void;
  markRead: (serviceId: string) => void;
  togglePause: (serviceId: string) => void;
  updateConfig: (cfg: AppConfig) => Promise<void>;
  removeProject: (projectId: string) => void;
  removeService: (serviceId: string) => void;
}

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

export const useStore = create<Store>((set, get) => {
  // 从 localStorage 恢复 openedTabs
  const loadOpenedTabs = (): string[] => {
    try {
      const raw = localStorage.getItem("javaboot:openedTabs");
      if (raw) {
        const arr = JSON.parse(raw);
        if (Array.isArray(arr)) return arr.filter((x: unknown) => typeof x === "string");
      }
    } catch {
      // ignore
    }
    return [];
  };
  const saveOpenedTabs = (tabs: string[]) => {
    try {
      localStorage.setItem("javaboot:openedTabs", JSON.stringify(tabs));
    } catch {
      // ignore
    }
  };

  // 批量 flush：把 pending 队列里的日志合并写入 store
  const flushLogs = () => {
    flushTimer = null;
    const pendingIds = Object.keys(pendingLogs);
    if (pendingIds.length === 0) return;
    const state = get();
    const maxLines = state.config.log_buffer_lines || 10000;
    const nextLogs = { ...state.logs };
    for (const sid of pendingIds) {
      const batch = pendingLogs[sid];
      if (!batch || batch.length === 0) continue;
      delete pendingLogs[sid];
      const existing = nextLogs[sid] ?? { lines: [], hasUnread: false };
      const lines = existing.lines;
      for (const l of batch) lines.push(l);
      if (lines.length > maxLines) {
        lines.splice(0, lines.length - maxLines);
      }
      const isSelected = state.selectedServiceId === sid;
      const isPaused = state.paused[sid];
      // 暂停的服务：日志已写入 lines 数组（缓存），但不生成新 LogBuffer 引用，
      // 避免触发订阅重渲染；恢复时会手动递增 logFlushTick 强制刷新。
      if (!isPaused) {
        nextLogs[sid] = { lines, hasUnread: !isSelected };
      }
    }
    set({ logs: nextLogs });
  };

  // 调度一次 flush（若已有定时器则复用，实现窗口内合并）
  const scheduleFlush = () => {
    if (flushTimer !== null) return;
    flushTimer = setTimeout(flushLogs, FLUSH_INTERVAL);
  };

  return {
  projects: [],
  services: [],
  runtimes: {},
  logs: {},
  paused: {},
  logFlushTick: 0,
  config: {
    port_refresh_interval_secs: 2,
    stop_on_compile_fail: false,
    auto_restart_debounce_secs: 3,
    log_buffer_lines: 10000,
    stop_all_on_exit: true,
  },
  selectedServiceId: null,
  openedTabs: loadOpenedTabs(),
  gitAvailable: false,
  loading: false,

  init: async () => {
    set({ loading: true });
    try {
      const [projects, services, runtimes, config, gitOk] = await Promise.all([
        api.listProjects(),
        api.listServices(),
        api.getAllRuntimes(),
        api.getConfig(),
        api.gitAvailable(),
      ]);
      const rtMap: Record<string, ServiceRuntime> = {};
      for (const r of runtimes) rtMap[r.service_id] = r;
      // 确保每个服务都有 runtime 记录
      for (const s of services) {
        if (!rtMap[s.id]) {
          rtMap[s.id] = {
            service_id: s.id,
            status: "stopped",
            pid: null,
            ports: [],
            service_ports: [],
            started_at: null,
            port_conflict: false,
            conflict_with: [],
            cpu_usage: null,
            memory_mb: null,
          };
        }
      }
      const logMap: Record<string, LogBuffer> = {};
      for (const s of services) {
        logMap[s.id] = { lines: [], hasUnread: false };
      }
      // 恢复持久化的 openedTabs，过滤掉已不存在的服务
      const serviceIds = new Set(services.map((s) => s.id));
      const persistedTabs = get().openedTabs.filter((id) => serviceIds.has(id));
      const openedTabs = persistedTabs.length > 0
        ? persistedTabs
        : services[0]
          ? [services[0].id]
          : [];
      saveOpenedTabs(openedTabs);
      set({
        projects,
        services,
        runtimes: rtMap,
        logs: logMap,
        config,
        gitAvailable: gitOk,
        loading: false,
        selectedServiceId: openedTabs[0] ?? null,
        openedTabs,
      });
    } catch (e) {
      console.error("init failed", e);
      set({ loading: false });
    }
  },

  refreshServices: async () => {
    const [projects, services] = await Promise.all([
      api.listProjects(),
      api.listServices(),
    ]);
    set({ projects, services });
  },

  selectService: (id) => {
    set((state) => {
      const nextOpened =
        id && !state.openedTabs.includes(id)
          ? [...state.openedTabs, id]
          : state.openedTabs;
      saveOpenedTabs(nextOpened);
      return { selectedServiceId: id, openedTabs: nextOpened };
    });
    if (id) get().markRead(id);
  },

  closeTab: (id) => {
    set((state) => {
      const idx = state.openedTabs.indexOf(id);
      if (idx < 0) return state;
      const nextOpened = state.openedTabs.filter((x) => x !== id);
      saveOpenedTabs(nextOpened);
      let nextSelected = state.selectedServiceId;
      if (state.selectedServiceId === id) {
        // 关的是当前选中：切到相邻 tab（优先右、否则左）
        nextSelected = nextOpened[idx] ?? nextOpened[idx - 1] ?? null;
      }
      return { openedTabs: nextOpened, selectedServiceId: nextSelected };
    });
  },

  setRuntime: (rt) => {
    set((state) => ({
      runtimes: { ...state.runtimes, [rt.service_id]: rt },
    }));
  },

  appendLog: (log) => {
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

  updateConfig: async (cfg) => {
    await api.saveConfig(cfg);
    set({ config: cfg });
  },

  removeProject: (projectId) => {
    set((state) => {
      const remainingServices = state.services.filter(
        (s) => s.project_id !== projectId
      );
      const remainingIds = new Set(remainingServices.map((s) => s.id));
      const remainingRuntimes: Record<string, ServiceRuntime> = {};
      const remainingLogs: Record<string, LogBuffer> = {};
      for (const s of remainingServices) {
        if (state.runtimes[s.id]) remainingRuntimes[s.id] = state.runtimes[s.id];
        if (state.logs[s.id]) remainingLogs[s.id] = state.logs[s.id];
      }
      const nextOpened = state.openedTabs.filter((id) => remainingIds.has(id));
      saveOpenedTabs(nextOpened);
      return {
        projects: state.projects.filter((p) => p.id !== projectId),
        services: remainingServices,
        runtimes: remainingRuntimes,
        logs: remainingLogs,
        openedTabs: nextOpened,
        selectedServiceId:
          state.selectedServiceId && remainingIds.has(state.selectedServiceId)
            ? state.selectedServiceId
            : nextOpened[0] ?? null,
      };
    });
  },

  removeService: (serviceId) => {
    // 清理 pending 队列
    delete pendingLogs[serviceId];
    set((state) => {
      const remainingServices = state.services.filter(
        (s) => s.id !== serviceId
      );
      const { [serviceId]: _, ...remainingRuntimes } = state.runtimes;
      const { [serviceId]: __, ...remainingLogs } = state.logs;
      const { [serviceId]: ___, ...remainingPaused } = state.paused;
      const nextOpened = state.openedTabs.filter((id) => id !== serviceId);
      saveOpenedTabs(nextOpened);
      return {
        services: remainingServices,
        runtimes: remainingRuntimes,
        logs: remainingLogs,
        paused: remainingPaused,
        openedTabs: nextOpened,
        selectedServiceId:
          state.selectedServiceId === serviceId
            ? nextOpened[0] ?? null
            : state.selectedServiceId,
      };
    });
  },
  };
});
