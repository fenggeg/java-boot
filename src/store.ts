import {create} from "zustand";
import type {AppConfig, DaemonHello, DaemonProcessInfo, LogLine, Project, Service, ServiceRuntime,} from "./types";
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
  loading: boolean;
  /** 初始化失败时的错误信息，UI 据此展示提示 */
  initError: string | null;
  // ---- P3 daemon 监控闭环 ----
  /** daemon 连接状态 */
  daemonConnected: boolean;
  /** daemon 握手信息 */
  daemonHello: DaemonHello | null;
  /** daemon 托管进程实时事实（run_id 键） */
  daemonProcesses: DaemonProcessInfo[];
  /** 最近一次指标刷新时间戳，UI 判断数据新鲜度 */
  daemonMetricsAt: number | null;

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
  // ---- P3 daemon 监控闭环 ----
  setDaemonConnected: (connected: boolean) => void;
  setDaemonHello: (hello: DaemonHello | null) => void;
  setDaemonProcesses: (list: DaemonProcessInfo[]) => void;
  updateDaemonMetrics: (runId: number, cpu: number | null, mem: number | null) => void;
  refreshDaemon: () => Promise<void>;
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
        nextLogs[sid] = { lines: trimmed, hasUnread: !isSelected };
      } else {
        // 暂停时也更新 lines 引用（新数组），但保持 hasUnread 不变
        nextLogs[sid] = { lines: trimmed, hasUnread: existing.hasUnread };
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
    dev_lazy_init: false,
  },
  selectedServiceId: null,
  openedTabs: loadOpenedTabs(),
  loading: false,
  initError: null,
  daemonConnected: false,
  daemonHello: null,
  daemonProcesses: [],
  daemonMetricsAt: null,

  init: async () => {
    set({ loading: true, initError: null });
    try {
      const [projects, services, runtimes, config] = await Promise.all([
        api.listProjects(),
        api.listServices(),
        api.getAllRuntimes(),
        api.getConfig(),
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
        loading: false,
        selectedServiceId: openedTabs[0] ?? null,
        openedTabs,
      });
    } catch (e) {
      const msg = api.toErrMsg(e);
      console.error("init failed", e);
      set({ loading: false, initError: msg });
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
    // 跳过已删除服务的 runtime 事件，避免"复活"
    const services = get().services;
    if (!services.some((s) => s.id === rt.service_id)) return;
    set((state) => ({
      runtimes: { ...state.runtimes, [rt.service_id]: rt },
    }));
  },

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
        const rt = state.runtimes[s.id];
        if (rt) remainingRuntimes[s.id] = rt;
        const lb = state.logs[s.id];
        if (lb) remainingLogs[s.id] = lb;
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

  setDaemonConnected: (connected) => set({ daemonConnected: connected }),
  setDaemonHello: (hello) => set({ daemonHello: hello }),
  setDaemonProcesses: (list) =>
    set({ daemonProcesses: list, daemonMetricsAt: Date.now() }),
  updateDaemonMetrics: (runId, cpu, mem) =>
    set((state) => {
      const idx = state.daemonProcesses.findIndex((p) => p.run_id === runId);
      const cur = state.daemonProcesses[idx];
      if (!cur) return {};
      const next = state.daemonProcesses.slice();
      const updated: DaemonProcessInfo = { ...cur, cpu_usage: cpu, memory_mb: mem };
      next[idx] = updated;
      return { daemonProcesses: next, daemonMetricsAt: Date.now() };
    }),
  refreshDaemon: async () => {
    const hello = await api.getDaemonHello();
    const connected = await api.getDaemonConnected();
    let list: DaemonProcessInfo[] = [];
    if (connected) {
      try {
        list = await api.reconcileDaemon();
      } catch {
        list = [];
      }
    }
    set({ daemonConnected: connected, daemonHello: hello, daemonProcesses: list, daemonMetricsAt: Date.now() });
  },
  };
});
