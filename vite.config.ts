import {defineConfig} from "vite";
import react from "@vitejs/plugin-react";
import path from "path";

// Tauri expects a fixed port 1420 in dev
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  server: {
    port: 1420,
    strictPort: true,
    host: "127.0.0.1",
    watch: {
      // Don't watch the Rust source
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    target: "chrome105",
    minify: "esbuild",
    sourcemap: false,
    rollupOptions: {
      output: {
        manualChunks: {
          // antd 体积大但变化少，独立 chunk 利用浏览器缓存
          antd: ["antd"],
          // React 运行时
          react: ["react", "react-dom"],
          // Tauri API 层
          tauri: ["@tauri-apps/api", "@tauri-apps/plugin-dialog", "@tauri-apps/plugin-opener"],
          // Monaco Editor 体积大且独立，独立 chunk 避免主包膨胀
          monaco: ["monaco-editor", "@monaco-editor/react"],
        },
      },
    },
  },
});
