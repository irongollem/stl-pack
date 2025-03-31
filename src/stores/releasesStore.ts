// src/stores/releasesStore.ts
import { defineStore } from 'pinia'
import { ref, computed } from 'vue'
import type { Release, StlModel } from '../bindings'
import {useToastStore} from "./toastStore.ts";


export type Tab = "settings" | "release" | "addStl";
export const useReleasesStore = defineStore('releases', () => {
    const toastStore = useToastStore();
    const release = ref<Release | undefined>();
    const activeTab = ref<Tab>('release');

    const setActiveTab = (tab: Tab) => {
        activeTab.value = tab;
    }

    const releaseExists = computed(() => !!release.value);
    const updateRelease = (newRelease: Release) => {
        release.value = {
            ...newRelease,
            models: [...(newRelease.models || [])]
        };
    }

    const addModel = (model: StlModel) => {
        if (!release.value) {
            toastStore.addToast(
                "Release not initialized. Please create a release first.",
                "error",
            )
            return;
        }
        if (!release.value.models) {
            release.value.models = [];
        }
        release.value.models.push(model)

    }
    const removeModel = (model: StlModel) => {
        if (!release.value || !release.value.models) return;

        const index = release.value.models
            .findIndex(m => m.model_name === model.model_name);
        if (index !== -1) {
            release.value.models.splice(index, 1);
        }
    }
    const modelCount = computed(() => release.value?.models?.length || 0);
    const clearModels = () => {
        if (!release.value) return;
        release.value.models = [];
    }

    return {
        activeTab,
        setActiveTab,
        release,
        releaseExists,
        updateRelease,
        modelCount,
        addModel,
        removeModel,
        clearModels
    }
})