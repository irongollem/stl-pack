<template>
  <View>
    <template #left>
        <h1 class="text-xl font-bold">Release info</h1>
        <TextInput id="release" label="Release" placeholder="Name of the release..." v-model="release.name" />
              
        <TextArea id="description" label="Description" placeholder="Enter the description..." v-model="release.description" />
      
        <MonthYearInput id="releaseDate" label="Release date" v-model="release.date" />
        
        <FileInput id="extraFiles" label="Additional content (pdf's etc.)" multiple accept=".zip,.rar,.7z, .pdf" v-model="extraFiles" />
        
        <FileInput id="releaseImages" label="Release images (group shots etc.)" multiple accept=".jpg,.jpeg,.png,.gif,.webp,image/*" v-model="releaseImages" />
    </template>
  </View>
</template>

<script setup lang="ts">
import { ref } from "vue";
import TextInput from "../components/TextInput.vue";
import MonthYearInput from "../components/MonthYearInput.vue";
import TextArea from "../components/TextArea.vue";
import FileInput from "../components/FileInput.vue";
import View from "../components/View.vue";

const release = ref({
  name: "",
  description: "",
  date: "",
  status: "planning",
  version: "1.0.0",
  stls: [],
});

const extraFiles = ref([]);
const releaseImages = ref([]);
</script>

