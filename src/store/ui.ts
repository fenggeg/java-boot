import type {StateCreator} from "zustand";
import type {Store, UiSlice} from "./types";

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

export const createUiSlice: StateCreator<Store, [], [], UiSlice> = (
  set,
  get
) => ({
  selectedServiceId: null,
  openedTabs: loadOpenedTabs(),
  loading: false,
  initError: null,

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
});
