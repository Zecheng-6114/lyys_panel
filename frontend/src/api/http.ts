import axios from "axios";
import router from "../router";

// 统一的 axios 实例：自动携带 JWT，401 时跳登录
const http = axios.create({
  baseURL: "/api",
  timeout: 15000,
});

http.interceptors.request.use((config) => {
  const token = localStorage.getItem("panel_token");
  if (token) {
    config.headers.Authorization = `Bearer ${token}`;
  }
  return config;
});

http.interceptors.response.use(
  (resp) => resp,
  (error) => {
    if (error.response?.status === 401) {
      localStorage.removeItem("panel_token");
      router.push("/login");
    }
    return Promise.reject(error);
  },
);

export default http;
