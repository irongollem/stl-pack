<script setup lang="ts">
import AddSTL from "./views/AddSTL.vue";
import { ref } from "vue";
import CreateRelease from "./views/CreateRelease.vue";
import Settings from "./views/Settings.vue";

const releaseExists = ref(false);
const activeTab = ref("release");
</script>

<template>
  <div class="h-screen flex flex-col overflow-hidden">

  <div class="tabs tabs-lift">
    <input
        type="radio"
        name="release"
        class="tab"
        :class="{ 'tab-active': activeTab === 'settings' }"
        @change="activeTab = 'settings'"
        aria-label="⚙️"
    />
    <div class="tab-content ">
      <Settings />
    </div>

    <input
        type="radio"
        name="release"
        class="tab"
        :class="{ 'tab-active': activeTab === 'release' }"
        @change="activeTab = 'release'"
        aria-label="Release"
        checked
    />
    <div class="tab-content h-full overflow-y-auto" role="tabpanel">
      <CreateRelease />
    </div>

    <input
        type="radio"
        name="release"
        class="tab"
        :class="{ 'tab-active': activeTab === 'addStl' }"
        @change="activeTab = 'addStl'"
        aria-label="Add STL"
        :disabled="releaseExists"
    />
    <div class="tab-content h-full overflow-y-auto" role="tabpanel">
      <AddSTL />
    </div>
  </div>
  </div>
</template>

<style scoped>
.tab-active {
  --tab-bg: rgb(31, 41, 55); /* Matches bg-gray-800 */
}
</style>
