<template>
  <div class="settings">
    <div class="card">
      <div class="card-title">AI API 配置</div>
      <div class="card-sub">
        用于 AI 助手对话的上游 OpenAI 兼容接口。留空的项回退到面板环境变量
        （AI_API_BASE / AI_API_KEY / AI_MODEL），均未配置时 AI 对话不可用。
      </div>

      <el-form label-position="top" class="form">
        <el-form-item label="上游 API 地址">
          <el-input
            v-model="form.base"
            placeholder="https://api.openai.com/v1（填到 /v1 为止）"
            clearable
          />
        </el-form-item>
        <el-form-item label="API 密钥">
          <el-input
            v-model="form.key"
            type="password"
            show-password
            autocomplete="new-password"
            :placeholder="keyPlaceholder"
          />
          <div v-if="config.keySet" class="key-hint">
            已保存密钥（{{ config.keyMasked }}）
            <el-button link type="danger" size="small" @click="clearKey">清除已存密钥</el-button>
          </div>
          <div v-else-if="config.envKeySet" class="key-hint">
            未在设置页保存密钥，当前使用环境变量 AI_API_KEY
          </div>
        </el-form-item>
        <el-form-item label="模型名">
          <el-input v-model="form.model" placeholder="gpt-4o-mini" clearable />
        </el-form-item>
        <el-form-item>
          <el-button type="primary" :loading="saving" @click="save">保存配置</el-button>
        </el-form-item>
      </el-form>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from "vue";
import { ElMessage } from "element-plus";
import http from "../api/http";

interface AiConfigResp {
  base: string;
  model: string;
  key_set: boolean;
  key_masked: string | null;
  env_key_set: boolean;
  configured: boolean;
}

// 生效配置（后端已做 settings → env → 默认值合并）
const config = reactive({
  base: "",
  model: "",
  keySet: false,
  keyMasked: null as string | null,
  envKeySet: false,
  configured: false,
});

// 表单：key 不回显明文，仅输入新密钥时提交
const form = reactive({ base: "", key: "", model: "" });
const saving = ref(false);

const keyPlaceholder = computed(() =>
  config.keySet ? "已配置，留空保持不变" : "sk-...",
);

async function load() {
  const { data } = await http.get<{ config: AiConfigResp }>("/ai/config");
  const c = data.config;
  config.base = c.base;
  config.model = c.model;
  config.keySet = c.key_set;
  config.keyMasked = c.key_masked;
  config.envKeySet = c.env_key_set;
  config.configured = c.configured;
  // 回显生效值；密钥不回显，占位符提示状态
  form.base = c.base;
  form.model = c.model;
  form.key = "";
}

async function save() {
  if (form.base.trim() && !/^https?:\/\//.test(form.base.trim())) {
    ElMessage.warning("上游 API 地址必须以 http(s):// 开头");
    return;
  }
  saving.value = true;
  try {
    await http.post("/ai/config", {
      base: form.base.trim(),
      model: form.model.trim(),
      // key 为空表示保持原密钥不变（后端 None=不改）
      ...(form.key.trim() ? { key: form.key.trim() } : {}),
    });
    ElMessage.success("AI API 配置已保存，对话即时生效");
    form.key = "";
    await load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.message || "保存失败");
  } finally {
    saving.value = false;
  }
}

async function clearKey() {
  saving.value = true;
  try {
    await http.post("/ai/config", { base: form.base.trim(), model: form.model.trim(), key: "" });
    ElMessage.success("已清除设置页密钥（如配置了环境变量将回退）");
    await load();
  } catch (e: any) {
    ElMessage.error(e.response?.data?.message || "操作失败");
  } finally {
    saving.value = false;
  }
}

onMounted(() => {
  load().catch(() => ElMessage.error("配置加载失败"));
});
</script>

<style scoped>
.settings {
  max-width: 640px;
}
.card {
  background: var(--el-bg-color);
  border-radius: var(--radius, 8px);
  padding: 20px 24px;
}
.card-title {
  font-size: 15px;
  font-weight: 600;
  margin-bottom: 4px;
}
.card-sub {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  margin-bottom: 16px;
  line-height: 1.6;
}
.form {
  margin-top: 8px;
}
.key-hint {
  font-size: 12px;
  color: var(--el-text-color-secondary);
  margin-top: 4px;
  display: flex;
  align-items: center;
  gap: 8px;
}
</style>
