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
  updateConfig: (cfg: AppConfig) => Promise<void>;
  removeProject: (projectId: string) => void;
  removeService: (serviceId: string) => void;
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

  return {
  projects: [],
  services: [],
  runtimes: {},
  logs: {},
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
    set((state) => {
      // 复用原数组引用做原地 push/splice，避免高频日志下每次 O(n) 拷贝
      const existing = state.logs[log.service_id] ?? {
        lines: [],
        hasUnread: false,
      };
      const lines = existing.lines;
      lines.push(log);
      // 超限裁剪：使用配置的 log_buffer_lines，配置缺失时回退 10000
      const maxLines = state.config.log_buffer_lines || 10000;
      if (lines.length > maxLines) {
        lines.splice(0, lines.length - maxLines);
      }
      const isSelected = state.selectedServiceId === log.service_id;
      return {
        logs: {
          ...state.logs,
          // 返回新 LogBuffer 对象（引用变化触发订阅），lines 数组保持同一引用
          [log.service_id]: {
            lines,
            hasUnread: !isSelected,
          },
        },
      };
    });
  },

  clearLog: (serviceId) => {
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
    set((state) => {
      const remainingServices = state.services.filter(
        (s) => s.id !== serviceId
      );
      const { [serviceId]: _, ...remainingRuntimes } = state.runtimes;
      const { [serviceId]: __, ...remainingLogs } = state.logs;
      const nextOpened = state.openedTabs.filter((id) => id !== serviceId);
      saveOpenedTabs(nextOpened);
      return {
        services: remainingServices,
        runtimes: remainingRuntimes,
        logs: remainingLogs,
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
