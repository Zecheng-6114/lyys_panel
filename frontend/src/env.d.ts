/// <reference types="vite/client" />

/// 由 vite.config.ts 从 backend/Cargo.toml 注入（见其中的 `define`）
declare const __APP_VERSION__: string;

declare module "*.vue" {
  import type { DefineComponent } from "vue";
  const component: DefineComponent<{}, {}, any>;
  export default component;
}
