<template>
  <label :for="id" class="floating-label mb-2">
    <span class="label"> {{ label }}</span>
    <textarea
        :id="id"
        class="textarea w-full"
        :value="modelValue"
        @input="$emit('update:modelValue', ($event.target as HTMLInputElement).value)"
        :placeholder="placeholder"
     />
  </label>
</template>

<script setup lang="ts">
defineProps<{
  id: string;
  label?: string;
  placeholder?: string;
  modelValue?: string | null;
}>();

defineEmits<{
  (e: "update:modelValue", value: string): void;
}>();
</script>