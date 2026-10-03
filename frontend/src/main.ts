import { createApp } from "vue";
import { createPinia } from "pinia";
import "element-plus/es/components/loading/style/css";
// 作业完成提示。样式得手写：解析器只给「没写 import、被自动导入」的名字补样式副作用，
// 显式 import 进来的名字不走它，样式不会进包（下面的 loading 是同一个原因）。
import "element-plus/es/components/notification/style/css";

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
