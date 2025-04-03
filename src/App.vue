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
    try {
      const result = await commands.finalizeRelease(
        releasesStore.release.release_dir,
      );
      if (result.status === "ok") {
        toastStore.addToast(
          "Release finalized and exported successfully",
          "success",
        );
      } else {
        console.error("Finalize release failed:", result.error);
        toastStore.addToast(
          `Failed to finalize release: ${
            typeof result.error === "string"
              ? result.error
              : JSON.stringify(result.error)
          }`,
          "error",
          0,
        );
      }
    } catch (error) {
      console.error("Finalize exception:", error);
      toastStore.addToast(
        `Exception during finalization: ${error}`,
        "error",
        0,
      );
    }
  }
};
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
                    @change="releasesStore.setActiveTab('release')"
                    aria-label="Release"
                    checked
                />
            </label>

            <input
                type="radio"
                name="release"
                class="tab"
                :class="{ 'tab-active': releasesStore.activeTab === 'addStl' }"
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
                @change="releasesStore.setActiveTab('finalize')"
                aria-label="Finalize"
                :disabled="!releasesStore.releaseExists"
            />
        </div>

        <!-- Tab content container - Reduced height to give space for rounded corners -->
        <div class="h-[calc(100vh-2rem)] overflow-hidden pb-4">
            <!-- Settings tab -->
            <div
                v-if="releasesStore.activeTab === 'settings'"
                class="h-[calc(100%-1rem)] bg-gray-800 rounded-b-2xl overflow-hidden"
            >
                <Settings />
            </div>

            <!-- Release tab -->
            <div
                v-if="releasesStore.activeTab === 'release'"
                class="h-[calc(100%-1rem)] bg-gray-800 rounded-b-2xl overflow-hidden"
            >
                <CreateRelease />
            </div>

            <!-- AddSTL tab -->
            <div
                v-if="releasesStore.activeTab === 'addStl'"
                class="h-[calc(100%-1rem)] bg-gray-800 rounded-b-2xl overflow-hidden"
            >
                <AddSTL />
            </div>

            <!-- Finalize tab -->
            <div
                v-if="releasesStore.activeTab === 'finalize'"
                class="h-[calc(100%-1rem)] bg-gray-800 rounded-b-2xl p-8"
            >
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
