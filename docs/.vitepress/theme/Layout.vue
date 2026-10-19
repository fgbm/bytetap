<script setup lang="ts">
import DefaultTheme from "vitepress/theme";
import SignalTrace from "./components/SignalTrace.vue";

const { Layout } = DefaultTheme;
</script>

<template>
  <Layout>
    <template #home-hero-image>
      <SignalTrace class="hero-trace" />
    </template>
  </Layout>
</template>
