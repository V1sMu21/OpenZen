import { defineConfig, type Plugin } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { rmSync } from "node:fs";
import { fileURLToPath } from "node:url";

/**
 * 宠物动画帧（public/pet/frames，~144MB 松散 PNG）不出现在构建产物里：
 * 源码原样保留供 dev 继续开发，release 打包体积保持轻量（设计文档要求
 * sprite sheet <300KB，帧数到位前先不入包）。closeBundle 在 public
 * 拷贝完成后运行，目录存在时才删除。
 */
function stripPetFramesPlugin(): Plugin {
  return {
    name: "strip-pet-frames",
    apply: "build",
    closeBundle() {
      const dir = fileURLToPath(new URL("dist/pet/frames", import.meta.url));
      try {
        rmSync(dir, { recursive: true, force: true });
        console.log("[strip-pet-frames] removed", dir);
      } catch (e) {
        console.warn("[strip-pet-frames] failed to remove", dir, e);
      }
    },
  };
}

export default defineConfig({
  plugins: [svelte(), stripPetFramesPlugin()],
  resolve: {
    extensions: [".svelte.ts", ".ts", ".js", ".svelte"],
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: false,
  },
  server: {
    port: 5173,
    strictPort: false,
    proxy: {
      "/api": {
        target: "http://localhost:18567",
        changeOrigin: true,
      },
    },
  },
});
