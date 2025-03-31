<script setup lang="ts">
import { useReleasesStore } from "../stores/releasesStore.ts";

const releasesStore = useReleasesStore();
</script>

<template>
  <label class="floating-label" for="modelOverview">
    <span class="label">Model overview</span>
  </label>
  <table id="modelOverview" class="table table-xs w-full" v-if="releasesStore.release?.models.length">
    <thead>
    <tr>
      <th>Group</th>
      <th>Model Name</th>
      <th>Actions</th>
    </tr>
    </thead>
    <tbody>
    <tr v-for="model in releasesStore.release.models" :key="model.model_name">
      <td>{{ model.group || "-" }}</td>
      <td class="max-w-[200px] truncate" :title="model.model_name">{{ model.model_name }}</td>
      <td>
      </td>
      <td>
        <button class="btn btn-error btn-xs" @click="releasesStore.removeModel(model)">
          Remove
        </button>
      </td>
    </tr>
    </tbody>
  </table>

</template>

<style scoped>

</style>