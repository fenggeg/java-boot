import type {StateCreator} from "zustand";
import * as api from "../api";
import type {DaemonSlice, Store} from "./types";

export const createDaemonSlice: StateCreator<
  Store,
  [],
  [],
  DaemonSlice
> = (set) => ({
  daemonConnected: false,
  daemonHello: null,
  daemonProcesses: [],
  daemonMetricsAt: null,

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
      const updated = { ...cur, cpu_usage: cpu, memory_mb: mem };
      next[idx] = updated;
      return { daemonProcesses: next, daemonMetricsAt: Date.now() };
    }),
  refreshDaemon: async () => {
    const hello = await api.getDaemonHello();
    const connected = await api.getDaemonConnected();
    let list = [] as Store["daemonProcesses"];
    if (connected) {
      try {
        list = await api.reconcileDaemon();
      } catch {
        list = [];
      }
    }
    set({
      daemonConnected: connected,
      daemonHello: hello,
      daemonProcesses: list,
      daemonMetricsAt: Date.now(),
    });
  },
});
