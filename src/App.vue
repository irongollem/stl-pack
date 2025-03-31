<script setup lang="ts">
import AddSTL from "./views/AddSTL.vue";
import CreateRelease from "./views/CreateRelease.vue";
import Settings from "./views/Settings.vue";
import ToastContainer from "./components/ToastContainer.vue";
import { useReleasesStore } from "./stores/releasesStore";

const releasesStore = useReleasesStore();
</script>

<template>
  <div class="h-screen flex flex-col overflow-hidden">
    <div class="tabs tabs-lift">
      <input
          type="radio"
          name="release"
          class="tab"
          :class="{ 'tab-active': releasesStore.activeTab === 'settings' }"
          @change="releasesStore.setActiveTab('settings')"
          aria-label="⚙️"
      />
      <div class="tab-content ">
        <Settings />
      </div>

      <label
          class="tab"
          :class="{ 'tab-active': releasesStore.activeTab === 'release' }"
      >
        <span class="tab-text mr-2">Release
</span>
        <span  class="indicator-item indicator-middle badge badge-primary">{{ releasesStore.modelCount }}</span>
        <input
            type="radio"
            name="release"
            @change="releasesStore.setActiveTab('release')"
            aria-label="Release"
            checked
        />
      </label>
      <div class="tab-content h-full overflow-y-auto" role="tabpanel">
        <CreateRelease />
      </div>

      <input
          type="radio"
          name="release"
          class="tab"
          :class="{ 'tab-active': releasesStore.activeTab === 'addStl' }"
          @change="releasesStore.setActiveTab('addStl')"
          aria-label="Add STL"
          :disabled="!releasesStore.releaseExists"
      />
      <div class="tab-content h-full overflow-y-auto" role="tabpanel">
        <AddSTL />
      </div>
    </div>
  </div>

  <ToastContainer />
</template>

<style scoped>
.tab-active {
  --tab-bg: rgb(31, 41, 55); /* Matches bg-gray-800 */
}
</style>