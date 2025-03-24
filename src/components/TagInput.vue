<template>
  <div class="form-control w-full mb-2">
    <label v-if="label" :for="id" class="floating-label">
      <span class="label-text">{{ label }}</span>

    <div class="textarea w-full flex flex-wrap gap-2">
      <div v-for="(tag, index) in modelValue" :key="index"
           class="badge badge-primary">
        {{ tag }}
        <button type="button" @click="() => removeTag(index)"
                class="btn btn-ghost btn-xs btn-circle">
          <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24"
               class="w-3 h-3 stroke-current">
            <path stroke-linecap="round" stroke-linejoin="round" stroke-width="3"
                  d="M6 18L18 6M6 6l12 12"></path>
          </svg>
        </button>
      </div>

      <input
          :id="id"
          v-model="inputValue"
          @keydown="handleKeydown"
          class="border-none outline-none focus:outline-none"
          type="text"
          :placeholder="modelValue.length ? '' : placeholder"
      />
    </div>
    </label>
  </div>
</template>

<script setup lang="ts">
import { ref } from "vue";

const props = defineProps<{
  id: string;
  label?: string;
  placeholder?: string;
  modelValue: string[];
}>();

const emit = defineEmits<{
  (e: "update:modelValue", value: string[]): void;
}>();

const inputValue = ref("");

const addTag = () => {
  const tag = inputValue.value.trim();
  if (tag && !props.modelValue.includes(tag)) {
    emit("update:modelValue", [...props.modelValue, tag]);
  }
  inputValue.value = "";
};

const removeTag = (index: number) => {
  const newTags = [...props.modelValue];
  newTags.splice(index, 1);
  emit("update:modelValue", newTags);
};

const handleKeydown = (e: KeyboardEvent) => {
  if (e.key === "Enter") {
    e.preventDefault();
    addTag();
  }
};
</script>

