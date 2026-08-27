import {create} from "zustand";

export type ThemeMode = "light" | "dark";

interface ThemeStore {
  mode: ThemeMode;
  toggle: () => void;
  setMode: (mode: ThemeMode) => void;
}

const raw =
  typeof localStorage !== "undefined"
    ? localStorage.getItem("javaboot:theme")
    : null;
const stored: ThemeMode | null =
  raw === "dark" || raw === "light" ? raw : null;

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
