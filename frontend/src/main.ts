import { createApp } from "vue";
import { createPinia } from "pinia";
import "element-plus/es/components/loading/style/css";

import App from "./App.vue";
import router from "./router";
import "./styles/theme.css";
// 4.1 移动端适配：表格列优先级折叠等断点样式
import "./styles/responsive.css";

const app = createApp(App);
app.use(createPinia());
app.use(router);
app.use(ElLoading);
app.mount("#app");
