<template>
  <label :for="id" class="floating-label mb-2">
    <span class="label"> {{ label }}</span>
    <input
        :id="id"
        :value="modelValue"
        class="input w-full"
        type="text"
        @input="$emit('update:modelValue', ($event.target as HTMLInputElement).value)"
        :placeholder="placeholder"
        :required="required"
     />
  </label>
</template>

<script setup lang="ts">
defineProps<{
  id: string;
  label?: string;
  placeholder?: string;
  modelValue?: string;
  required?: boolean;
}>();

defineEmits<{
  (e: "update:modelValue", value: string): void;
}>();
</script>