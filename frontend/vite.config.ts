import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
import AutoImport from "unplugin-auto-import/vite";
import { ElementPlusResolver } from "unplugin-vue-components/resolvers";
import Components from "unplugin-vue-components/vite";

// 版本号以 backend/Cargo.toml 为唯一来源，构建时注入。
// 前端硬编码会随发布逐渐与后端脱节，这里读一次更省事。
// 正则锚定行首，`[dependencies]` 里的 `version = "..."` 都嵌在行内，不会误匹配。
function cargoVersion(): string {
  const toml = readFileSync(
    fileURLToPath(new URL("../backend/Cargo.toml", import.meta.url)),
    "utf8",
  );
  const m = toml.match(/^version\s*=\s*"([^"]+)"/m);
  if (!m) throw new Error("无法从 backend/Cargo.toml 读取 version");
  return m[1];
}

// 开发时把 /api 代理到后端，生产构建产物由后端 rust-embed 内嵌
export default defineConfig({
  // 相对基路径：产物里资源用 ./ 引用，服务端按需注入 <base href="/<安全入口>/">，
  // 因此在任意入口前缀下都能正确解析（见 backend/src/embed.rs）。
  base: "./",
  plugins: [
    vue(),
    // element-plus 按需引入：模板里的 el-* 组件与 ElMessage 等 API 只打包用到的那部分，
    // 替代 main.ts 里的全量引入。dts 放 src/ 里让 vue-tsc 能读到。
    AutoImport({ resolvers: [ElementPlusResolver()], dts: "src/auto-imports.d.ts" }),
    Components({ resolvers: [ElementPlusResolver()], dts: false }),
  ],
  define: {
    __APP_VERSION__: JSON.stringify(cargoVersion()),
  },
  // 生产构建去掉 console/debugger 调用（esbuild 压缩阶段生效）
  esbuild: {
    drop: ["console", "debugger"],
  },
  server: {
    port: 5173,
    proxy: {
      "/api": {
        target: "http://127.0.0.1:3789",
        changeOrigin: true,
      },
    },
  },
  build: {
    outDir: "dist",
    rollupOptions: {
      output: {
        // 把体积大且很少变动的第三方库拆成独立 chunk，便于长期缓存
        manualChunks(id) {
          if (id.includes("node_modules/echarts") || id.includes("node_modules/zrender")) {
            return "echarts";
          }
          if (id.includes("node_modules/element-plus")) {
            return "element-plus";
          }
          // 取色算法（量化器 + HCT）只在设置页点「从背景图取色」时才用得上，
          // 单独成块才能真的按需加载 —— 并进 vendor 的话，它会跟着入口的静态
          // 依赖一起进首屏，dynamic import 就白写了。
          if (id.includes("node_modules/@material/material-color-utilities")) {
            return "monet";
          }
          if (id.includes("node_modules")) {
            return "vendor";
          }
        },
      },
    },
  },
});
