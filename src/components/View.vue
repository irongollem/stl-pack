<template>
    <main
        class="bg-gray-800 text-gray-100 flex flex-col md:flex-row gap-8 p-8 pb-2 h-full rounded-b-lg"
    >
        <section class="md:w-1/2 overflow-y-auto pr-2 max-h-full mb-8">
            <slot name="left"></slot>
        </section>
        <aside class="md:w-1/2 overflow-y-auto pl-2 max-h-full mb-8">
            <slot name="right"></slot>
        </aside>
    </main>
</template>
