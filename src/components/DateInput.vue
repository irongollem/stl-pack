<template>
  <label :for="id">
    {{ label }}
  </label>

  <button
      :id="id"
      type="button"
      :popovertarget="`${id}-popover`"
      class="input input-border"
      style="anchor-name:--cally-anchor">
    {{ formattedDate || placeholder }}
  </button>
  <div
      popover
      :id="`${id}-popover`"
      class="dropdown bg-base-100 rounded-box shadow-lg"
      style="position-anchor:--cally-anchor">
    <calendar-date class="cally" @change="handleDateChange">
    <svg aria-label="Previous" class="fill-current size-4" slot="previous" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M15.75 19.5 8.25 12l7.5-7.5"></path></svg>
    <svg aria-label="Next" class="fill-current size-4" slot="next" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="m8.25 4.5 7.5 7.5-7.5 7.5"></path></svg>
    <calendar-month>
    </calendar-month>
    </calendar-date>
  </div>
</template>

<script setup lang="ts">
import "cally";
import { computed } from "vue";

const props = defineProps<{
  id: string;
  label: string;
  placeholder: string;
  modelValue: string;
}>();

const emit = defineEmits<{
  "update:modelValue": [value: string];
}>();

const formattedDate = computed(() => {
  if (!props.modelValue) {
    return "";
  }
  const selectedDate = new Date(props.modelValue);
  return new Intl.DateTimeFormat("en", {
    month: "long",
    year: "numeric",
  }).format(selectedDate);
});
const handleDateChange = (e: CustomEvent) => {
  const date = (e.target as HTMLInputElement).value;

  if (date) {
    emit("update:modelValue", date);
  }
};
</script>
