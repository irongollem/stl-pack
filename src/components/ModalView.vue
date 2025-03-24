<script setup lang="ts">
import { ref, onMounted, onBeforeUnmount } from 'vue';

const props = defineProps<{
  isOpen: boolean;
}>();

const emit = defineEmits<{
  (e: 'close', value: boolean): void;
}>();

// Add keyboard support to close modal with Escape key
onMounted(() => {
  window.addEventListener('keydown', handleKeyDown);
});

onBeforeUnmount(() => {
  window.removeEventListener('keydown', handleKeyDown);
});

const closeModal = () => {
  console.log('closeModal emitting');
  emit("close", false);
}

const handleKeyDown = (e: KeyboardEvent) => {
  if (e.key === 'Escape' && props.isOpen) {
    console.log('Escape key pressed');
    closeModal();
  }
};
</script>

<template>
<div v-if="isOpen" class="fixed inset-0 bg-black/75 flex items-center justify-center z-50" @click="closeModal">
  <div class="relative max-w-4xl max-h-screen p-4" @click.stop>
    <slot></slot>
    <button
        @click="closeModal"
        class="absolute top-2 right-2 bg-red-500 text-white rounded-full w-8 h-8 flex items-center justify-center hover:bg-red-700 text-xl font-bold"
    >
      ×
    </button>
  </div>
</div>

</template>
