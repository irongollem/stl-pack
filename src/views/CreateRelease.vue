<template>
  <form @submit.prevent="saveRelease">
    <View>
      <template #left>
        <h1 class="text-xl font-bold">Release info</h1>
        <TextInput
          id="designer"
          label="Designer"
          placeholder="Name of the designer..."
          v-model="release.designer"
          required
        />

        <TextInput
          id="release"
          label="Release"
          placeholder="Name of the release..."
          v-model="release.name"
          required
        />

        <MonthYearInput
          id="releaseDate"
          label="Release date"
          v-model="release.date"
          required
        />

        <TextArea
          id="description"
          label="Description"
          placeholder="Enter the description (Optional)..."
          v-model="release.description"
        />
        <ModelOverview v-if="release.models.length > 0" />
        <FileSelect
          id="extraFiles"
          label="Additional content (licence, pdf's etc.)"
          multiple
          accept="pdf, md, zip"
          v-model="extraFiles"
        />

        <div class="flex justify-between w-full mb-4">
          <button
            class="btn"
            type="submit"
            :disabled="!formComplete || isStoring"
          >
            <template v-if="isStoring">
              <span class="loading loading-spinner"></span>
              <span>Storing...</span>
            </template>
            <span v-else>Create Release</span>
          </button>
          <button class="btn btn-error" @click="clearRelease">
            Clear Release
          </button>
        </div>
      </template>

      <template #right>
        <ImageSelect v-model="releaseImages" />
      </template>
    </View>
  </form>
</template>

<script setup lang="ts">
import { computed, ref } from "vue";
import { openPath } from "@tauri-apps/plugin-opener";

import { commands, type Release } from "../bindings";
import TextInput from "../components/TextInput.vue";
import MonthYearInput from "../components/MonthYearInput.vue";
import TextArea from "../components/TextArea.vue";
import View from "../components/View.vue";
import ImageSelect from "../components/ImageSelect.vue";
import ModelOverview from "../components/ModelOverview.vue";
import FileSelect from "../components/FileSelect.vue";
import { useToastStore } from "../stores/toastStore.ts";
import { useReleasesStore } from "../stores/releasesStore.ts";
import type { SelectedFile } from "../composables/useFileSelect";

const toastStore = useToastStore();
const releasesStore = useReleasesStore();
const release = ref<Release>({
  name: "",
  designer: "",
  description: "",
  date: "",
  version: "1.0.0",
  models: [],
  release_dir: "",
  images: [],
  other_files: [],
});
const extraFiles = ref<SelectedFile[]>([]);
const releaseImages = ref<SelectedFile[]>([]);

const isStoring = ref(false);

const clearRelease = () => {
  release.value = {
    name: "",
    designer: "",
    description: "",
    date: "",
    version: "1.0.0",
    models: [],
    release_dir: "",
    images: [],
    other_files: [],
  };
  extraFiles.value = [];
  releaseImages.value = [];
};

const formComplete = computed(
  () => release.value.name && release.value.designer && release.value.date,
);

const saveRelease = async () => {
  if (!formComplete.value) {
    toastStore.addToast("Please enter a name for the release", "error", 0);
    return;
  }
  isStoring.value = true;
  const dirName = formatDirName(release.value);
  release.value.release_dir = dirName;
  const result = await commands.createRelease(
    release.value,
    releaseImages.value.map((image) => image.path),
    extraFiles.value.map((file) => file.path),
  );
  if (result.status === "ok") {
    releasesStore.updateRelease(release.value);
    releasesStore.setActiveTab("addStl");
    // TODO: Check if this works, possibly sends back a relative path
    await openPath(result.data);
  } else {
    toastStore.addToast(
      `Failed to create release: ${result.error}`,
      "error",
      0,
    );
  }
  isStoring.value = false;
};

const formatDirName = (release: Release) => {
  const cleanDesignerName = release.designer
    .toLowerCase()
    .replace(/\s+/g, "-")
    .replace(/[^\w-]/g, "");

  const cleanName = release.name
    .toLowerCase()
    .replace(/\s+/g, "-")
    .replace(/[^\w-]/g, "");

  const [month, year] = release.date.split("/");

  return `${cleanDesignerName} - ${month.padStart(2, "0")}-${year} - ${cleanName}`;
};
</script>
