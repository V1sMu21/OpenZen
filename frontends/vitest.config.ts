import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";

/**
 * 前端单元测试配置（round5 P1-13④）。
 *
 * 只做**纯逻辑**测试：SSE 事件归并、streamEvents → parts 转换、markdown
 * 转义、artifactType 判定。不引入 jsdom / playwright —— 组件渲染与真实
 * 交互仍由 tauri-e2e 技能覆盖，这里守的是"最易回归且能纯函数验证"的那段。
 *
 * 需要 svelte 插件的原因：`sse.ts → chat.ts → sidepanel.svelte.ts` 这条
 * import 链上有 `.svelte.ts`（$state rune）文件，必须经编译器处理。
 */
export default defineConfig({
  plugins: [svelte({ hot: false })],
  resolve: {
    extensions: [".svelte.ts", ".ts", ".js", ".svelte"],
  },
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
    // 测试文件自己管好副作用；任何未处理的 rejection 都应当让 CI 变红。
    dangerouslyIgnoreUnhandledErrors: false,
  },
});
