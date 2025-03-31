<script setup lang="ts">
import AddSTL from "./views/AddSTL.vue";
import CreateRelease from "./views/CreateRelease.vue";
import Settings from "./views/Settings.vue";
import ToastContainer from "./components/ToastContainer.vue";
import { useReleasesStore } from "./stores/releasesStore";
import { useToastStore } from "./stores/toastStore.ts";
import { commands } from "./bindings.ts";

const releasesStore = useReleasesStore();
const toastStore = useToastStore();

const finalizeRelease = async () => {
  if (releasesStore.release) {
    const result = await commands.finalizeRelease(releasesStore.release.name);
    if (result.status === "ok") {
      toastStore.addToast(
        "Release finalized and exported successfully",
        "success",
      );
    } else {
      toastStore.addToast(
        "Failed to finalize release: " + result.error,
        "error",
        0,
      );
    }
  }
};
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

      <input
          type="radio"
          name="finalize"
          class="tab"
          :class="{ 'tab-active': releasesStore.activeTab === 'finalize' }"
          @change="releasesStore.setActiveTab('finalize')"
          aria-label="Finalize"
          :disabled="!releasesStore.releaseExists"
      />
      <div class="tab-content h-full overflow-y-auto" role="tabpanel">
        <button
            class="btn btn-success"
            @click="finalizeRelease"
            :disabled="!releasesStore.modelCount"
        >
          Finalize & Export Release
        </button>
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