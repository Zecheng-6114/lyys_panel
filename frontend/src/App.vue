<template>
  <!-- Element Plus 自带文案默认英文：表格空态「No Data」、分页「Total」、
       日期选择器、下拉的「No data」都会漏出来，整页中文里夹着这几个词很扎眼。
       在这一层统一切到中文，各页不必各自处理。 -->
  <el-config-provider :locale="zhCn">
    <router-view />
  </el-config-provider>
</template>

<script setup lang="ts">
import zhCn from "element-plus/es/locale/lang/zh-cn";
</script>
