<template>
  <main class="bg-gray-800 h-screen overflow-y-auto text-gray-100 flex flex-col md:flex-row gap-8 p-8">
    <section class="md:w-1/2">
      <slot name="left"></slot>
    </section>
    <aside class="md:w-1/2">
      <slot name="right"></slot>
    </aside>
  </main>
</template>
