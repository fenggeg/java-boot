import {create} from "zustand";
import * as api from "../api";
import type {LogLine, ServiceRuntime} from "../types";
import {createDaemonSlice} from "./daemon";
import {createLogsSlice} from "./logs";
import {createServicesSlice} from "./services";
import type {LogBuffer, Store} from "./types";
import {createUiSlice} from "./ui";

/** 跨域编排：初始化拉取 projects/services/runtimes/config，并恢复 openedTabs */
async function initStore(
  set: (partial: Partial<Store>) => void,
  get: () => Store,
) {
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
    try {
      localStorage.setItem("javaboot:openedTabs", JSON.stringify(openedTabs));
    } catch {
      // ignore
    }
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
}

export const useStore = create<Store>((...a) => {
  const [set, get] = a;
  return {
    ...createServicesSlice(...a),
    ...createLogsSlice(...a),
    ...createDaemonSlice(...a),
    ...createUiSlice(...a),

    init: async () => initStore(set, get),

    setRuntime: (rt: ServiceRuntime) => {
      // 跳过已删除服务的 runtime 事件，避免"复活"
      const services = get().services;
      if (!services.some((s) => s.id === rt.service_id)) return;
      set((state) => ({
        runtimes: { ...state.runtimes, [rt.service_id]: rt },
      }));
    },
  };
});

// 兼容旧引用路径：部分代码可能直接 import type { LogBuffer } from store
export type { LogBuffer, Store };
export type { LogLine };
