<template>
  <form @submit.prevent="saveRelease">
    <View>
      <template #left>
          <h1 class="text-xl font-bold">Release info</h1>
          <TextInput id="designer" label="Designer" placeholder="Name of the designer..." v-model="release.designer" required />

          <TextInput id="release" label="Release" placeholder="Name of the release..." v-model="release.name" required />

          <MonthYearInput id="releaseDate" label="Release date" v-model="release.date" required />

          <TextArea id="description" label="Description" placeholder="Enter the description (Optional)..." v-model="release.description" />

          <FileInput id="extraFiles" label="Additional content (pdf's etc.)" multiple accept=".zip,.rar,.7z, .pdf" v-model="extraFiles" />

        <div class="flex justify-between w-full mb-4">
          <button class="btn" type="submit" :disabled="!formComplete || isStoring">
            <template v-if="isStoring">
              <span class="loading loading-spinner"></span>
              <span>Storing...</span>
            </template>
            <span v-else>Save Model</span>
          </button>
          <button class="btn btn-error" @click="clearRelease">Clear Release</button>
        </div>
      </template>

      <template #right>
        <ImagePreview
            v-model="releaseImages"
        />
      </template>
    </View>
  </form>
</template>

<script setup lang="ts">
import {computed, ref} from "vue";
import TextInput from "../components/TextInput.vue";
import MonthYearInput from "../components/MonthYearInput.vue";
import TextArea from "../components/TextArea.vue";
import FileInput from "../components/FileInput.vue";
import View from "../components/View.vue";
import ImagePreview from "../components/ImagePreview.vue";
import {commands, Release} from "../bindings"
import {useToastStore} from "../stores/toastStore.ts";
import {useReleasesStore} from "../stores/releasesStore.ts";

const toastStore = useToastStore();
const releasesStore = useReleasesStore();
const release = ref<Release>({
  name: "",
  designer: "",
  description: "",
  date: "",
  version: "1.0.0",
  models: [],
});
const extraFiles = ref<File[]>([]);
const releaseImages = ref<File[]>([]);

const isStoring = ref(false);

const clearRelease = () => {
  release.value = {
    name: "",
    designer: "",
    description: "",
    date: "",
    version: "1.0.0",
    models: [],
  };
  extraFiles.value = [];
  releaseImages.value = [];
};

const formComplete = computed(() =>
    release.value.name &&
    release.value.designer &&
    release.value.date
);

const saveRelease = async () => {
  if (!formComplete.value) {
    toastStore.addToast (
      "Please enter a name for the release",
      "error",
      0
    );
    return;
  }
  isStoring.value = true;

  const result = await commands.createRelease(release.value);
  if (result.status === "ok") {
    releasesStore.updateRelease(release.value);
    releasesStore.setActiveTab("addStl");
  } else {
    toastStore.addToast(
      "Failed to create release: " + result.error,
      "error",
        0
    );
  }
  isStoring.value = false;
}
</script>

