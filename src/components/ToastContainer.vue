<script setup lang="ts">
import { useToastStore } from "../stores/toastStore.ts";

const toastStore = useToastStore();
</script>

<template>
  <div class="fixed bottom-0 right-0 p-4 z-50">
    <transition-group name="toast">
      <div
          v-for="toast in toastStore.toasts"
          :key="toast.id"
          class="alert mb-2 flex justify-between shadow-lg"
          :class="`alert-${toast.type}`"
      >
        <span>{{ toast.message }}</span>
        <button
            @click="toastStore.removeToast(toast.id)"
            class="btn btn-circle btn-sm"
        >
          ×
        </button>
      </div>
    </transition-group>
  </div>
</template>

<style scoped>
.toast-enter-active,
.toast-leave-active {
  transition: all 0.3s ease;
}

.toast-enter-from {
  opacity: 0;
  transform: translateY(30px);
}

.toast-leave-to {
  opacity: 0;
  transform: translateX(100%);
}
</style>