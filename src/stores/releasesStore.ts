// src/stores/releasesStore.ts
import { defineStore } from "pinia";
import { ref, computed } from "vue";
import type { Release, StlModel } from "../bindings";
import { useToastStore } from "./toastStore.ts";

export type Tab = "settings" | "release" | "addStl" | "finalize";
export const useReleasesStore = defineStore("releases", () => {
  const toastStore = useToastStore();
  const release = ref<Release | undefined>();
  const models = ref<StlModel[]>([]);
  const releaseDir = ref<string | undefined>();
  const activeTab = ref<Tab>("release");

  const setActiveTab = (tab: Tab) => {
    activeTab.value = tab;
  };

  const releaseExists = computed(() => !!release.value);
  const updateRelease = (newRelease: Release) => {
    release.value = {
      ...newRelease,
      models: [...(newRelease.models || [])],
    };
  };

  const addModel = (model: StlModel, path: string) => {
    if (!release.value) {
      toastStore.addToast(
        "Release not initialized. Please create a release first.",
        "error",
      );
      return;
    }

    models.value.push(model);
    release.value.models.push(path);
  };

  const removeModel = (model: StlModel) => {
    if (!release.value || !release.value.models || !models.value) return;

    const index = models.value.findIndex(
      (m) => m.model_name === model.model_name,
    );
    if (index !== -1) {
      models.value.splice(index, 1);
      release.value.models.splice(index, 1);
    }
  };

  const modelCount = computed(() => release.value?.models?.length || 0);

  const clearModels = () => {
    if (!release.value) return;
    release.value.models = [];
    models.value = [];
  };

  return {
    activeTab,
    setActiveTab,
    release,
    releaseDir,
    releaseExists,
    updateRelease,
    models,
    modelCount,
    addModel,
    removeModel,
    clearModels,
  };
});
