import { createApp } from "vue";
import { createPinia } from "pinia";
import "element-plus/es/components/loading/style/css";

import App from "./App.vue";
import router from "./router";
import "./styles/theme.css";

const app = createApp(App);
app.use(createPinia());
app.use(router);
app.use(ElLoading);
app.mount("#app");
