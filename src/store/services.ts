import type {StateCreator} from "zustand";
import * as api from "../api";
import {dropPendingLogs} from "./logs";
import type {ServicesSlice, Store} from "./types";

const DEFAULT_CONFIG = {
  port_refresh_interval_secs: 2,
  stop_on_compile_fail: false,
  auto_restart_debounce_secs: 3,
  log_buffer_lines: 10000,
  stop_all_on_exit: true,
  dev_lazy_init: false,
} as const;

export const createServicesSlice: StateCreator<
  Store,
  [],
  [],
  ServicesSlice
> = (set) => ({
  projects: [],
  services: [],
  runtimes: {},
  config: { ...DEFAULT_CONFIG },

  refreshServices: async () => {
    const [projects, services] = await Promise.all([
      api.listProjects(),
      api.listServices(),
    ]);
    set({ projects, services });
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
      const remainingRuntimes: Store["runtimes"] = {};
      const remainingLogs: Store["logs"] = {};
      for (const s of remainingServices) {
        const rt = state.runtimes[s.id];
        if (rt) remainingRuntimes[s.id] = rt;
        const lb = state.logs[s.id];
        if (lb) remainingLogs[s.id] = lb;
      }
      const nextOpened = state.openedTabs.filter((id) => remainingIds.has(id));
      // 同步 persist（与 ui 切片共用 localStorage 协议）
      try {
        localStorage.setItem("javaboot:openedTabs", JSON.stringify(nextOpened));
      } catch {
        // ignore
      }
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
    dropPendingLogs(serviceId);
    set((state) => {
      const remainingServices = state.services.filter(
        (s) => s.id !== serviceId
      );
      const { [serviceId]: _, ...remainingRuntimes } = state.runtimes;
      const { [serviceId]: __, ...remainingLogs } = state.logs;
      const { [serviceId]: ___, ...remainingPaused } = state.paused;
      const nextOpened = state.openedTabs.filter((id) => id !== serviceId);
      try {
        localStorage.setItem("javaboot:openedTabs", JSON.stringify(nextOpened));
      } catch {
        // ignore
      }
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
});
