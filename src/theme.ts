import {create} from "zustand";

export type ThemeMode = "light" | "dark";

interface ThemeStore {
  mode: ThemeMode;
  toggle: () => void;
  setMode: (mode: ThemeMode) => void;
}

const stored =
  typeof localStorage !== "undefined"
    ? (localStorage.getItem("javaboot:theme") as ThemeMode | null)
    : null;

export const useThemeStore = create<ThemeStore>((set) => ({
  mode: stored === "dark" ? "dark" : "light",
  toggle: () =>
    set((s) => {
      const next: ThemeMode = s.mode === "light" ? "dark" : "light";
      try {
        localStorage.setItem("javaboot:theme", next);
      } catch {
        // ignore
      }
      return { mode: next };
    }),
  setMode: (mode) =>
    set(() => {
      try {
        localStorage.setItem("javaboot:theme", mode);
      } catch {
        // ignore
      }
      return { mode };
    }),
}));
