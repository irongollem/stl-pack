<script setup lang="ts">
defineProps<{
  modelValue: boolean;
  label: string;
}>();

defineEmits<{
  "update:modelValue": [value: boolean];
}>();
</script>

<template>
  <fieldset class="fieldset p-2 bg-base-100 border border-gray-500 rounded-box w-full mb-2">
    <label class="fieldset-label">
      <input type="checkbox"
            class="toggle"
            :checked="modelValue"
            @input="$emit('update:modelValue', ($event.target as HTMLInputElement).checked)"
            />
      {{ label }}
    </label>
  </fieldset>
</template>
