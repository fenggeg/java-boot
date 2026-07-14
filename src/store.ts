import { create } from "zustand";
import type {
  Project,
  Service,
  ServiceRuntime,
  AppConfig,
  LogLine,
  ServiceStatus,
} from "./types";
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
  gitAvailable: boolean;
  loading: boolean;

  // actions
  init: () => Promise<void>;
  refreshServices: () => Promise<void>;
  selectService: (id: string | null) => void;
  setRuntime: (rt: ServiceRuntime) => void;
  appendLog: (log: LogLine) => void;
  clearLog: (serviceId: string) => void;
  markRead: (serviceId: string) => void;
  updateConfig: (cfg: AppConfig) => Promise<void>;
  removeProject: (projectId: string) => void;
  removeService: (serviceId: string) => void;
}

const MAX_LOG_LINES = 10000;

export const useStore = create<Store>((set, get) => ({
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
      set({
        projects,
        services,
        runtimes: rtMap,
        logs: logMap,
        config,
        gitAvailable: gitOk,
        loading: false,
        selectedServiceId: services[0]?.id ?? null,
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
    set({ selectedServiceId: id });
    if (id) get().markRead(id);
  },

  setRuntime: (rt) => {
    set((state) => ({
      runtimes: { ...state.runtimes, [rt.service_id]: rt },
    }));
  },

  appendLog: (log) => {
    set((state) => {
      const existing = state.logs[log.service_id] ?? {
        lines: [],
        hasUnread: false,
      };
      const lines = [...existing.lines, log];
      // 超限裁剪
      const trimmed =
        lines.length > MAX_LOG_LINES
          ? lines.slice(lines.length - MAX_LOG_LINES)
          : lines;
      const isSelected = state.selectedServiceId === log.service_id;
      return {
        logs: {
          ...state.logs,
          [log.service_id]: {
            lines: trimmed,
            hasUnread: isSelected ? false : true,
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
      const remainingRuntimes: Record<string, ServiceRuntime> = {};
      const remainingLogs: Record<string, LogBuffer> = {};
      for (const s of remainingServices) {
        if (state.runtimes[s.id]) remainingRuntimes[s.id] = state.runtimes[s.id];
        if (state.logs[s.id]) remainingLogs[s.id] = state.logs[s.id];
      }
      return {
        projects: state.projects.filter((p) => p.id !== projectId),
        services: remainingServices,
        runtimes: remainingRuntimes,
        logs: remainingLogs,
        selectedServiceId:
          state.selectedServiceId &&
          remainingServices.some((s) => s.id === state.selectedServiceId)
            ? state.selectedServiceId
            : remainingServices[0]?.id ?? null,
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
      return {
        services: remainingServices,
        runtimes: remainingRuntimes,
        logs: remainingLogs,
        selectedServiceId:
          state.selectedServiceId === serviceId
            ? remainingServices[0]?.id ?? null
            : state.selectedServiceId,
      };
    });
  },
}));
