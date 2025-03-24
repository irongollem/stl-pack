<template>
  <label :for="id" class="floating-label mb-2">
    <span class="label">{{ label }}</span>

    <div class="input flex space-between p-0 w-full">
      <select
          v-model="selectedMonth"
          class="select focus:outline-none focus:border-none border-none bg-transparent"
          @change="updateValue">
        <option value="" disabled>Month</option>
        <option v-for="(month, index) in months" :key="index+1" :value="index+1">
          {{ month }}
        </option>
      </select>

      <select
          v-model="selectedYear"
          class="select focus:outline-none focus:border-none border-none bg-transparent"
          @change="updateValue">
        <option value="" disabled>Year</option>
        <option v-for="year in years" :key="year" :value="year">
          {{ year }}
        </option>
      </select>
    </div>
  </label>
</template>

<script setup lang="ts">
import { ref, computed, onMounted } from "vue";

const props = defineProps<{
  id: string;
  label?: string;
  modelValue?: string;
}>();

const emit = defineEmits<{
  (e: "update:modelValue", value: string): void;
}>();

// Generate month names
const months = [
  "January",
  "February",
  "March",
  "April",
  "May",
  "June",
  "July",
  "August",
  "September",
  "October",
  "November",
  "December",
];

// Generate year range from 2018 to current year
const currentYear = new Date().getFullYear();
const years = computed(() => {
  const yearRange = [];
  for (let year = 2018; year <= currentYear; year++) {
    yearRange.push(year);
  }
  return yearRange;
});

// Track selected values
const selectedMonth = ref("");
const selectedYear = ref("");

// Parse initial value if provided
onMounted(() => {
  if (props.modelValue) {
    const parts = props.modelValue.split("/");
    if (parts.length === 2) {
      const month = parseInt(parts[0]);
      const year = parseInt(parts[1]);

      if (!isNaN(month) && month >= 1 && month <= 12) {
        selectedMonth.value = month.toString();
      }

      if (!isNaN(year)) {
        selectedYear.value = year.toString();
      }
    }
  }
});

// Update the model value when selections change
const updateValue = () => {
  if (selectedMonth.value && selectedYear.value) {
    emit("update:modelValue", `${selectedMonth.value}/${selectedYear.value}`);
  } else {
    emit("update:modelValue", "");
  }
};
</script>