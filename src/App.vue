<script setup lang="ts">
import AddSTL from "./views/AddSTL.vue";
import CreateRelease from "./views/CreateRelease.vue";
import Settings from "./views/Settings.vue";
import ToastContainer from "./components/ToastContainer.vue";
import { useReleasesStore } from "./stores/releasesStore";

import Finalize from "./views/Finalize.vue";

import { computed } from "vue";
const releasesStore = useReleasesStore();

const currentTabComponent = computed(() => {
  switch (releasesStore.activeTab) {
    case "settings":
      return Settings;
    case "release":
      return CreateRelease;
    case "addStl":
      return AddSTL;
    case "finalize":
      return Finalize;
    default:
      return CreateRelease;
  }
});
</script>

<template>
    <div class="h-screen flex flex-col">
        <!-- Tab headers -->
        <div class="tabs tabs-lift">
            <input
                type="radio"
                name="release"
                class="tab"
                :class="{
                    'tab-active': releasesStore.activeTab === 'settings',
                }"
                :checked="releasesStore.activeTab === 'settings'"
                @change="releasesStore.setActiveTab('settings')"
                aria-label="⚙️"
            />

            <label
                class="tab"
                :class="{ 'tab-active': releasesStore.activeTab === 'release' }"
            >
                <span class="tab-text mr-2">Release</span>
                <span
                    class="indicator-item indicator-middle badge badge-primary"
                    >{{ releasesStore.modelCount }}</span
                >
                <input
                    type="radio"
                    name="release"
                    :checked="releasesStore.activeTab === 'release'"
                    @change="releasesStore.setActiveTab('release')"
                    aria-label="Release"
                />
            </label>

            <input
                type="radio"
                name="release"
                class="tab"
                :class="{ 'tab-active': releasesStore.activeTab === 'addStl' }"
                :checked="releasesStore.activeTab === 'addStl'"
                @change="releasesStore.setActiveTab('addStl')"
                aria-label="Add STL"
                :disabled="!releasesStore.releaseExists"
            />

            <input
                type="radio"
                name="finalize"
                class="tab"
                :class="{
                    'tab-active': releasesStore.activeTab === 'finalize',
                }"
                :checked="releasesStore.activeTab === 'finalize'"
                @change="releasesStore.setActiveTab('finalize')"
                aria-label="Finalize"
                :disabled="!releasesStore.releaseExists || !releasesStore.models.length"
            />
        </div>

        <div class="h-[calc(100vh-2rem)] overflow-hidden pb-4">
          <div class="h-[calc(100%-1rem)] bg-gray-800 rounded-b-2xl overflow-hidden">
                          <KeepAlive>
                              <component
                                  :is="currentTabComponent"
                                  class="h-full"
                              />
                          </KeepAlive>
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
