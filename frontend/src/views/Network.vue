<template>
  <div class="network">
    <div class="toolbar">
      <el-radio-group v-model="tab">
        <el-radio-button value="interfaces">接口</el-radio-button>
        <el-radio-button value="connections">连接</el-radio-button>
        <el-radio-button value="routes">路由</el-radio-button>
        <el-radio-button value="dns">DNS</el-radio-button>
      </el-radio-group>
      <div class="spacer"></div>
      <el-button @click="load">刷新</el-button>
    </div>

    <el-table
      v-if="tab === 'interfaces'"
      v-loading="loading"
      :data="ifaces"
      size="small"
      class="ntable"
      height="var(--panel-table-height)"
    >
      <el-table-column label="接口" prop="ifname" width="140" />
      <el-table-column label="状态" width="100">
        <template #default="{ row }">
          <span class="dot" :class="row.operstate === 'UP' ? 'dot-on' : 'dot-off'"></span>
          {{ row.operstate }}
        </template>
      </el-table-column>
      <el-table-column label="MTU" prop="mtu" width="90" />
      <el-table-column label="类型" prop="link_type" width="120" />
      <el-table-column label="MAC" width="180">
        <template #default="{ row }">{{ row.address || "-" }}</template>
      </el-table-column>
      <el-table-column label="IP 地址" min-width="280">
        <template #default="{ row }">
          <div v-for="(a, i) in row.addr_info || []" :key="i">
            {{ a.local }}/{{ a.prefixlen }}
            <span class="fam">{{ a.family === "inet6" ? "v6" : "v4" }}</span>
          </div>
          <span v-if="!row.addr_info || row.addr_info.length === 0">-</span>
        </template>
      </el-table-column>
    </el-table>

    <el-table
      v-else-if="tab === 'connections'"
      v-loading="loading"
      :data="conns"
      size="small"
      class="ntable"
      height="var(--panel-table-height)"
    >
      <el-table-column label="状态" prop="state" width="130" />
      <el-table-column label="本地地址" prop="local" min-width="200" />
      <el-table-column label="对端地址" prop="peer" min-width="200" />
      <el-table-column label="队列(R/S)" width="110">
        <template #default="{ row }">{{ row.recv_q }} / {{ row.send_q }}</template>
      </el-table-column>
      <el-table-column label="进程" min-width="220">
        <template #default="{ row }">{{ row.process || "-" }}</template>
      </el-table-column>
    </el-table>

    <el-table
      v-else-if="tab === 'routes'"
      v-loading="loading"
      :data="routes"
      size="small"
      class="ntable"
      height="var(--panel-table-height)"
    >
      <el-table-column label="目的" min-width="200">
        <template #default="{ row }">{{ row.dst || "default" }}</template>
      </el-table-column>
      <el-table-column label="网关" width="180">
        <template #default="{ row }">{{ row.gateway || "-" }}</template>
      </el-table-column>
      <el-table-column label="接口" prop="dev" width="140" />
      <el-table-column label="协议" prop="protocol" width="110" />
      <el-table-column label="scope" prop="scope" width="110" />
    </el-table>

    <div v-else class="dnsbox">
      <div v-if="dns.length === 0" class="empty">未配置 nameserver</div>
      <div v-for="(d, i) in dns" :key="i" class="dnsrow">{{ d }}</div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { onMounted, ref } from "vue";
import http from "../api/http";

interface Iface {
  ifname: string;
  operstate: string;
  mtu: number;
  link_type: string;
  address?: string;
  addr_info?: { local: string; prefixlen: number; family: string }[];
}
interface Conn {
  state: string;
  recv_q: string;
  send_q: string;
  local: string;
  peer: string;
  process: string;
}
interface Route {
  dst?: string;
  gateway?: string;
  dev: string;
  protocol: string;
  scope: string;
}

const tab = ref<"interfaces" | "connections" | "routes" | "dns">("interfaces");
const loading = ref(false);
const ifaces = ref<Iface[]>([]);
const conns = ref<Conn[]>([]);
const routes = ref<Route[]>([]);
const dns = ref<string[]>([]);

async function load() {
  loading.value = true;
  try {
    if (tab.value === "interfaces") {
      const { data } = await http.get("/network/interfaces");
      ifaces.value = data;
    } else if (tab.value === "connections") {
      const { data } = await http.get("/network/connections");
      // 监听中的排前面，其余按本地端口排序
      conns.value = [...data].sort(
        (a, b) =>
          Number(b.state === "LISTEN") - Number(a.state === "LISTEN") ||
          String(a.local).localeCompare(String(b.local))
      );
    } else if (tab.value === "routes") {
      const { data } = await http.get("/network/routes");
      routes.value = data;
    } else {
      const { data } = await http.get("/network/dns");
      dns.value = data;
    }
  } catch (e: any) {
    ElMessage.error(e.response?.data?.error ?? "读取网络信息失败");
  } finally {
    loading.value = false;
  }
}

onMounted(load);
</script>

<style scoped>
.fam {
  font-size: 11px;
  color: var(--el-text-color-secondary);
  margin-left: 4px;
}
.dnsbox {
  background: var(--el-bg-color);
  border-radius: var(--radius);
  padding: 12px 16px;
}
.dnsrow {
  font-family: var(--panel-mono);
  font-size: 13px;
  padding: 4px 0;
}
.empty {
  color: var(--el-text-color-secondary);
  font-size: 13px;
}
</style>
