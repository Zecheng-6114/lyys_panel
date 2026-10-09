<template>
  <div class="network">
    <div class="toolbar">
      <el-radio-group v-model="tab" ref="tabGroup">
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
      <el-table-column label="接口" prop="ifname" v-bind="col(140)" />
      <el-table-column label="状态" v-bind="col(100)">
        <template #default="{ row }">
          <span class="dot" :class="row.operstate === 'UP' ? 'dot-on' : 'dot-off'"></span>
          {{ row.operstate }}
        </template>
      </el-table-column>
      <el-table-column label="MTU" prop="mtu" v-bind="col(90)" v-if="!hideColP3" />
      <el-table-column label="类型" prop="link_type" v-bind="col(120)" v-if="!hideColP2" />
      <el-table-column label="MAC" v-bind="col(180)" v-if="!hideColP3">
        <template #default="{ row }">{{ row.address || "-" }}</template>
      </el-table-column>
      <el-table-column label="IP 地址" v-bind="col(280, true)">
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
      <el-table-column label="状态" prop="state" v-bind="col(130)" />
      <el-table-column label="本地地址" prop="local" v-bind="col(200, true)" />
      <el-table-column label="对端地址" prop="peer" v-bind="col(200, true)" v-if="!hideColP2" />
      <el-table-column label="队列(R/S)" v-bind="col(110)" v-if="!hideColP3">
        <template #default="{ row }">{{ row.recv_q }} / {{ row.send_q }}</template>
      </el-table-column>
      <el-table-column label="进程" v-bind="col(220, true)">
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
      <el-table-column label="目的" v-bind="col(200, true)">
        <template #default="{ row }">{{ row.dst || "default" }}</template>
      </el-table-column>
      <el-table-column label="网关" v-bind="col(180)">
        <template #default="{ row }">{{ row.gateway || "-" }}</template>
      </el-table-column>
      <el-table-column label="接口" prop="dev" v-bind="col(140)" />
      <el-table-column label="协议" prop="protocol" v-bind="col(110)" v-if="!hideColP2" />
      <el-table-column label="scope" prop="scope" v-bind="col(110)" v-if="!hideColP3" />
    </el-table>

    <div v-else class="dnsbox">
      <div v-if="dns.length === 0" class="empty">未配置 nameserver</div>
      <div v-for="(d, i) in dns" :key="i" class="dnsrow">{{ d }}</div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { col, hideColP2, hideColP3 } from "../composables/useResponsive";
import { useSegmentIndicator } from "../composables/useSegmentIndicator";
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
/// 分段单选的滑动指示块（详见 composable）
const tabGroup = ref<HTMLElement | null>(null);
useSegmentIndicator(tabGroup);
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
  font-size: var(--fs-xs);
  color: var(--el-text-color-secondary);
  margin-left: var(--sp-1);
}
.dnsbox {
  background: var(--panel-card-bg, var(--el-bg-color));
  border-radius: var(--radius);
  padding: var(--sp-3) var(--sp-4);
}
.dnsrow {
  font-family: var(--panel-mono);
  font-variant-numeric: tabular-nums;
  font-size: var(--fs-base);
  padding: var(--sp-1) 0;
}
.empty {
  color: var(--el-text-color-secondary);
  font-size: var(--fs-base);
}
</style>
