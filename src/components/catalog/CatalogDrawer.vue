<template>
  <!-- Drag handle to widen the drawer at the list's expense — long file
       paths and the details grid want more room than a fixed width -->
  <div
    v-if="selectedGroup"
    class="w-1.5 shrink-0 cursor-col-resize rounded-full bg-base-content/5 hover:bg-primary/40 transition-colors"
    title="Drag to resize"
    @mousedown.prevent="startDrawerResize"
  ></div>

  <!-- Detail drawer: keyed on the GROUP so switching cards swaps the
       content in place — keying on the loaded entry unmounted the whole
       drawer during the members fetch and the layout flashed -->
  <aside
    v-if="selectedGroup"
    :style="{ width: `${drawerWidth}px` }"
    class="shrink-0 overflow-y-auto"
  >
    <div v-if="!selected" class="h-40 flex items-center justify-center">
      <div
        v-if="drawerLoadError"
        class="flex flex-col items-center gap-2 px-5 text-center"
      >
        <span class="font-mono text-[10.5px] text-base-content/50">
          {{ drawerLoadError }}
        </span>
        <button
          type="button"
          class="btn btn-xs"
          @click="selectedGroup && selectGroup(selectedGroup)"
        >
          Try again
        </button>
      </div>
      <span v-else class="loading loading-spinner loading-sm opacity-40"></span>
    </div>
    <template v-else>
      <!-- Picture area: preview image, or the 3D viewport inline when
         toggled (no more full-screen overlay) -->
      <div
        class="relative aspect-4/3 rounded-box bg-base-300 border border-base-content/10 flex items-center justify-center text-base-content/30 overflow-hidden"
      >
        <StlViewport
          v-if="show3d && stlPaths.length"
          :parts="stlPaths"
          compact
        />
        <div
          v-else-if="viewer3dBusy"
          class="absolute inset-0 flex items-center justify-center gap-2 bg-base-300/70 font-mono text-[10.5px]"
        >
          <span class="loading loading-spinner loading-xs"></span>
          unpacking…
        </div>
        <img
          v-else-if="drawerPreview"
          :src="convertFileSrc(drawerPreview)"
          :alt="selected.name"
          class="w-full h-full object-cover cursor-zoom-in"
          title="Click to view large"
          @click="showImageModal = true"
        />
        <span v-else class="text-5xl">🗿</span>
        <button
          v-if="!show3d"
          type="button"
          class="absolute bottom-1.5 right-1.5 btn btn-xs bg-base-100/70"
          @click="pickPreviewImage"
        >
          set image…
        </button>
        <button
          v-if="!show3d && drawerPreview"
          type="button"
          class="absolute bottom-1.5 left-1.5 btn btn-xs bg-base-100/70"
          title="Use this variant's image as the card image in the catalog"
          @click="useAsCardImage"
        >
          ★ card image
        </button>
        <button
          v-if="show3d"
          type="button"
          class="absolute top-1.5 right-1.5 btn btn-xs bg-base-100/70"
          title="Open large viewer"
          @click="show3dModal = true"
        >
          ⤢
        </button>
      </div>

      <!-- Primary actions ride right under the preview they act on.
           On a packed member the needed files are extracted from the
           archive just-in-time (ensure_model_files) — the archive stays
           authoritative and cleanup takes the copies back. -->
      <div class="flex gap-1.5 mt-2">
        <button
          type="button"
          class="flex-1 text-center font-semibold text-[11px] tracking-wider bg-primary text-primary-content rounded-md py-2 cursor-pointer disabled:opacity-40"
          :title="
            selected.packed
              ? 'Files extract from the archive automatically'
              : undefined
          "
          @click="printModel"
        >
          PRINT
        </button>
        <button
          type="button"
          class="flex-1 text-center font-semibold text-[11px] tracking-wider border rounded-md py-2 cursor-pointer disabled:opacity-40"
          :class="
            show3d ? 'border-primary text-primary' : 'border-base-content/15'
          "
          :disabled="!stlPaths.length || viewer3dBusy"
          :title="
            selected.packed
              ? 'Files extract from the archive automatically'
              : undefined
          "
          @click="toggle3d"
        >
          3D
        </button>
        <button
          type="button"
          class="flex-1 text-center font-semibold text-[11px] tracking-wider border border-base-content/15 rounded-md py-2 cursor-pointer disabled:opacity-40"
          :disabled="!stlPaths.length"
          :title="
            selected.packed
              ? 'Files extract from the archive automatically'
              : undefined
          "
          @click="renderSelected"
        >
          RENDER
        </button>
      </div>

      <div class="py-3.5 flex flex-col gap-2.5">
        <div>
          <!-- Group title: the logical model; rename applies to the whole
             group and survives rescans -->
          <div class="flex items-start gap-1.5">
            <h2
              v-if="!renamingGroup"
              class="font-bold text-[16px] leading-tight flex-1 flex items-center gap-1.5"
            >
              {{ selectedGroup?.group_name ?? selected.name }}
              <span
                v-if="selectedGroup?.nsfw"
                class="badge badge-xs badge-error badge-outline font-mono"
                title="Hidden from browsing unless Show 18+ is on in Settings"
                >18+</span
              >
            </h2>
            <form
              v-else
              class="flex-1 flex gap-1"
              @submit.prevent="renameGroup"
            >
              <input
                v-model="groupNameDraft"
                type="text"
                class="input input-xs font-mono flex-1"
                placeholder="empty = folder name"
              />
              <button type="submit" class="btn btn-xs btn-primary">save</button>
            </form>
            <button
              v-if="!renamingGroup"
              type="button"
              class="text-xs opacity-40 hover:opacity-100 cursor-pointer"
              title="Rename this model (all variants move with it; naming it like another model merges them)"
              @click="startRenameGroup"
            >
              ✎
            </button>
          </div>
          <div v-if="!renamingGroup && groupSources.length > 1" class="mt-0.5">
            <button
              type="button"
              class="font-mono text-[10px] text-primary/70 hover:text-primary cursor-pointer"
              :title="`Combined from: ${groupSources.join(', ')} — click to split them apart again`"
              @click="splitGroup"
            >
              combined from {{ groupSources.length }} models · split
            </button>
          </div>
          <p
            v-if="selectedGroup?.designer || selectedGroup?.release_name"
            class="font-mono text-[11px] text-base-content/50 mt-0.5"
          >
            {{
              [selectedGroup?.designer, selectedGroup?.release_name]
                .filter(Boolean)
                .join(" · ")
            }}
          </p>
        </div>

        <!-- Variant navigation: supported/unsupported tabs, poses within -->
        <div
          v-if="supportTabs.length > 1"
          class="flex bg-base-200 border border-base-content/10 rounded-lg p-0.5"
        >
          <button
            v-for="tab in supportTabs"
            :key="tab"
            type="button"
            class="flex-1 font-semibold text-[11px] px-2 py-1 rounded-md cursor-pointer"
            :class="
              activeSupport === tab
                ? 'bg-primary text-primary-content'
                : 'text-base-content/60'
            "
            @click="setSupportTab(tab)"
          >
            {{ tabLabel(tab) }}
          </button>
        </div>
        <!-- variant tier: shown only when a build has more than one -->
        <div v-if="variantsInTab.length > 1" class="flex flex-wrap gap-1.5">
          <button
            v-for="variant in variantsInTab"
            :key="variant"
            type="button"
            class="font-mono text-[11px] rounded-md px-2.5 py-1 border cursor-pointer"
            :class="
              activeVariant === variant
                ? 'bg-primary text-primary-content border-primary'
                : 'text-base-content/60 border-base-content/15'
            "
            @click="setVariant(variant)"
          >
            {{ variantLabel(variant) }}
          </button>
        </div>
        <!-- pose tier: the members within the active (support, variant) -->
        <div v-if="tabMembers.length > 1" class="flex flex-wrap gap-1.5">
          <button
            v-for="member in tabMembers"
            :key="memberKey(member)"
            type="button"
            class="font-mono text-[11px] rounded-full px-2.5 py-1 border cursor-pointer"
            :class="
              memberKey(member) === memberKey(selected)
                ? 'bg-primary text-primary-content border-primary'
                : 'text-base-content/60 border-base-content/15'
            "
            @click="selectEntry(member)"
          >
            {{ member.pose || member.name }}
          </button>
        </div>

        <!-- Everything here belongs to the selected physical build, so it
             sits after the build navigation. The model identity above stays
             fixed while Supported/Unsupported changes this small region. -->
        <div class="border-l-2 border-base-content/10 pl-2 min-w-0">
          <div class="flex items-center gap-1.5 min-w-0">
            <span
              class="font-mono font-semibold text-[9px] tracking-[0.1em] text-base-content/30 shrink-0"
            >
              SELECTED BUILD
            </span>
            <span
              v-if="measuredLabel"
              class="font-mono text-[9.5px] text-base-content/40 truncate"
              title="Measured from the geometry when this build was rendered"
            >
              · {{ measuredLabel }}
            </span>
          </div>
          <div class="flex items-center gap-x-2 min-w-0">
            <button
              type="button"
              class="min-w-0 truncate font-mono text-[10px] text-base-content/45 cursor-pointer hover:text-base-content/75 text-left"
              :title="`${selected.dir_path} — click to reveal`"
              @click="reveal(selected.dir_path)"
            >
              {{ displayPath }}
            </button>
            <button
              v-if="
                groupSources.length > 1 &&
                selected.source_group.toLowerCase() !==
                  selectedGroup?.group_name.toLowerCase()
              "
              type="button"
              class="font-mono text-[9.5px] text-error/60 hover:text-error cursor-pointer shrink-0"
              :title="`Pull “${selected.source_group}” back out of this model — the rest stays combined`"
              @click="detachSelectedSource"
            >
              detach source
            </button>
          </div>
        </div>

        <div class="flex flex-wrap gap-1.5">
          <span
            v-for="tag in selected.tags"
            :key="tag"
            class="font-mono text-[10px] text-base-content/60 border border-base-content/15 rounded-full px-2.5 py-0.5 flex items-center gap-1"
          >
            {{ tag }}
            <button
              type="button"
              class="opacity-50 hover:opacity-100"
              @click="removeTag(tag)"
            >
              ✕
            </button>
          </span>
          <form class="join" @submit.prevent="addTag">
            <input
              v-model="newTag"
              type="text"
              class="input input-xs join-item w-24"
              placeholder="+ tag"
            />
          </form>
        </div>

        <!-- Model details (pose/scale/supports/release date) -->
        <div>
          <div
            class="font-mono font-semibold text-[9.5px] tracking-[0.12em] text-base-content/40 mb-1.5"
          >
            DETAILS
          </div>
          <div class="grid grid-cols-2 gap-1.5">
            <label class="flex flex-col gap-0.5 col-span-2">
              <span class="font-mono text-[9px] text-base-content/40"
                >NAME</span
              >
              <input
                v-model="metaDraft.name"
                type="text"
                class="input input-xs font-mono"
                placeholder="model name"
              />
            </label>
            <label class="flex flex-col gap-0.5">
              <span class="font-mono text-[9px] text-base-content/40"
                >DESIGNER</span
              >
              <input
                v-model="metaDraft.designer"
                type="text"
                class="input input-xs font-mono"
                placeholder="studio / brand"
              />
            </label>
            <label class="flex flex-col gap-0.5">
              <span class="font-mono text-[9px] text-base-content/40"
                >SCULPTOR</span
              >
              <input
                v-model="metaDraft.sculptor"
                type="text"
                class="input input-xs font-mono"
                placeholder="artist (if known)"
              />
            </label>
            <label class="flex flex-col gap-0.5">
              <span class="font-mono text-[9px] text-base-content/40"
                >VARIANT</span
              >
              <input
                v-model="metaDraft.variant"
                type="text"
                class="input input-xs font-mono"
                placeholder="e.g. sword, mounted"
              />
            </label>
            <label class="flex flex-col gap-0.5">
              <span class="font-mono text-[9px] text-base-content/40"
                >POSE</span
              >
              <input
                v-model="metaDraft.pose"
                type="text"
                class="input input-xs font-mono"
                placeholder="e.g. A"
              />
            </label>
            <label class="flex flex-col gap-0.5">
              <span class="font-mono text-[9px] text-base-content/40"
                >SCALE</span
              >
              <input
                v-model="metaDraft.scale"
                type="text"
                class="input input-xs font-mono"
                placeholder="e.g. 32mm"
              />
            </label>
            <label class="flex flex-col gap-0.5">
              <span class="font-mono text-[9px] text-base-content/40"
                >SUPPORTS</span
              >
              <select
                v-model="metaDraft.support_status"
                class="select select-xs font-mono"
              >
                <option value="">unknown</option>
                <option value="supported">supported</option>
                <option value="unsupported">unsupported</option>
                <option value="both">both</option>
              </select>
            </label>
            <label class="flex flex-col gap-0.5">
              <span class="font-mono text-[9px] text-base-content/40"
                >RELEASED</span
              >
              <input
                v-model="metaDraft.release_date"
                type="text"
                class="input input-xs font-mono"
                placeholder="YYYY-MM"
              />
            </label>
            <label class="flex flex-col gap-0.5">
              <span
                class="font-mono text-[9px] text-base-content/40"
                title="Round/oval base in millimetres — just numbers: 25, or 60x35 for an oval"
                >BASE ROUND MM</span
              >
              <input
                v-model="metaDraft.base_round_mm"
                type="text"
                class="input input-xs font-mono"
                placeholder="25 or 60x35"
              />
            </label>
            <label class="flex flex-col gap-0.5">
              <span
                class="font-mono text-[9px] text-base-content/40"
                title="Square/rectangular base in millimetres — just numbers: 25, or 50x25 for a rectangle"
                >BASE SQUARE MM</span
              >
              <input
                v-model="metaDraft.base_square_mm"
                type="text"
                class="input input-xs font-mono"
                placeholder="25 or 50x25"
              />
            </label>
            <label class="flex flex-col gap-0.5 col-span-2">
              <span class="font-mono text-[9px] text-base-content/40"
                >RELEASE</span
              >
              <input
                v-model="metaDraft.release_name"
                type="text"
                class="input input-xs font-mono"
                placeholder="e.g. Order of the Unicorn"
              />
            </label>
          </div>
          <button
            v-if="metaDirty"
            type="button"
            class="btn btn-xs btn-primary w-full mt-1.5"
            @click="saveMetadata"
          >
            Save details
          </button>
        </div>

        <!-- Folder layout and compression are two states in the same library
             lifecycle. Keeping them together also makes the required
             unpack → clean up → repack sequence visible. -->
        <div
          class="rounded-lg border"
          :class="
            metaDirty || structureClean === false
              ? 'border-warning/30 bg-warning/5'
              : 'border-base-content/10 bg-base-200/60'
          "
        >
          <div class="flex items-center gap-2.5 px-2.5 py-2">
            <span
              class="size-6 shrink-0 rounded-full flex items-center justify-center text-[11px]"
              :class="
                structureClean === true && !metaDirty
                  ? 'bg-success/12 text-success'
                  : metaDirty || structureClean === false
                    ? 'bg-warning/15 text-warning'
                    : 'bg-base-content/8 text-base-content/35'
              "
            >
              <span
                v-if="
                  (!metaDirty && structureClean === null) || refreshingSidecars
                "
                class="loading loading-spinner loading-xs"
              ></span>
              <template v-else>{{
                structureClean && !metaDirty ? "✓" : "!"
              }}</template>
            </span>
            <div class="min-w-0 flex-1">
              <div
                class="font-mono font-semibold text-[9px] tracking-[0.11em] text-base-content/35"
              >
                FOLDER STRUCTURE
              </div>
              <div
                class="text-[11px] leading-tight"
                :class="
                  metaDirty || structureClean === false
                    ? 'text-warning'
                    : 'text-base-content/65'
                "
              >
                {{
                  refreshingSidecars
                    ? "Updating metadata…"
                    : metaDirty
                      ? "Edits will change the target path"
                      : structureClean === null
                        ? "Checking folder layout…"
                        : structureClean
                          ? "Matches model metadata"
                          : "Doesn’t match model metadata"
                }}
              </div>
            </div>
            <button
              v-if="metaDirty || structureClean === false"
              type="button"
              class="btn btn-xs btn-ghost px-2 text-[10px] text-warning"
              :disabled="isPacking"
              :title="
                packedDirs.length
                  ? 'Packed folders must be unpacked before their structure can change'
                  : 'Save these details, then review the file moves needed to match them'
              "
              @click="
                packedDirs.length
                  ? unpackSelectedGroup()
                  : cleanUpSelectedGroup()
              "
            >
              {{ packedDirs.length ? "Unpack first" : "Clean up…" }}
            </button>
          </div>

          <div class="mx-2.5 h-px bg-base-content/8"></div>

          <div class="flex items-center gap-2.5 px-2.5 py-2">
            <span
              class="size-6 shrink-0 rounded-full flex items-center justify-center font-mono text-[12px] bg-base-content/8 text-base-content/45"
            >
              <span
                v-if="isPacking"
                class="loading loading-spinner loading-xs"
              ></span>
              <template v-else>{{ packedDirs.length ? "▣" : "□" }}</template>
            </span>
            <div class="min-w-0 flex-1">
              <div
                class="font-mono font-semibold text-[9px] tracking-[0.11em] text-base-content/35"
              >
                STORAGE
              </div>
              <div
                class="font-mono text-[10.5px] leading-tight text-base-content/60 truncate"
              >
                <template v-if="isPacking">
                  {{ packJobLabel }}
                  <template v-if="packProgress">
                    · {{ packProgress.percent }}%
                  </template>
                </template>
                <template v-else-if="packedDirs.length && packableDirs.length">
                  {{ packedDirs.length }} of
                  {{ packedDirs.length + packableDirs.length }} folders
                  compressed
                </template>
                <template v-else-if="packedDirs.length">
                  Compressed at rest
                </template>
                <template v-else-if="packableDirs.length">
                  Loose files · ready to compress
                </template>
                <template v-else>No packable files</template>
              </div>
            </div>
            <button
              v-if="isPacking"
              type="button"
              class="btn btn-xs btn-ghost px-2 text-[10px]"
              @click="cancelPack"
            >
              Cancel
            </button>
            <template v-else>
              <button
                v-if="packableDirs.length"
                type="button"
                class="btn btn-xs btn-ghost px-2 text-[10px]"
                :title="
                  packedDirs.length
                    ? 'Compress the remaining folders'
                    : 'Compress this model to save disk space'
                "
                @click="packSelectedGroup"
              >
                {{ packedDirs.length ? "Pack rest" : "Pack" }}
              </button>
              <button
                v-if="packedDirs.length"
                type="button"
                class="btn btn-xs btn-ghost px-2 text-[10px]"
                @click="unpackSelectedGroup"
              >
                Unpack
              </button>
            </template>
          </div>
        </div>

        <!-- A compact action rail, not another full-width panel. The wrapper
             around the disabled release action keeps its cursor and tooltip:
             disabled buttons themselves do not reliably receive either. -->
        <div class="flex items-center gap-1 min-h-7">
          <span
            class="font-mono font-semibold text-[9px] tracking-[0.11em] text-base-content/30 mr-auto"
          >
            MODEL ACTIONS
          </span>
          <span
            :class="releasesStore.releaseExists ? '' : 'cursor-not-allowed'"
            :title="
              releasesStore.releaseExists
                ? 'Add every available variant of this model to the active release'
                : 'Start or resume a release before adding models'
            "
          >
            <button
              type="button"
              class="btn btn-xs btn-ghost min-h-7 h-7 gap-1 px-2 font-mono text-[10px] font-medium text-primary disabled:pointer-events-none disabled:opacity-30"
              :disabled="!releasesStore.releaseExists"
              @click="addToDraftRelease"
            >
              <span class="text-sm leading-none">＋</span>
              Release
              <span
                v-if="releasesStore.modelCount"
                class="badge badge-xs badge-primary badge-outline"
              >
                {{ releasesStore.modelCount }}
              </span>
            </button>
          </span>

          <details class="dropdown dropdown-end shrink-0">
            <summary
              class="btn btn-xs btn-ghost min-h-7 h-7 gap-1 px-2 font-mono text-[10px] font-medium text-base-content/50 hover:text-base-content list-none [&::-webkit-details-marker]:hidden"
              title="More model actions"
            >
              More
              <span class="text-[8px] opacity-60">▾</span>
            </summary>
            <ul
              class="dropdown-content menu z-30 mt-1 w-64 rounded-lg border border-base-content/15 bg-base-100 p-1.5 shadow-xl"
            >
              <li>
                <button
                  type="button"
                  class="items-start gap-2 py-2 disabled:opacity-40"
                  :disabled="refreshingSidecars"
                  @click="
                    closeActionMenu($event);
                    refreshSidecars([selectedGroup?.group_name ?? '']);
                  "
                >
                  <span class="mt-0.5 text-base-content/40">↻</span>
                  <span class="flex flex-col items-start">
                    <span class="text-[11px] font-semibold">
                      {{
                        refreshingSidecars
                          ? "Rebuilding metadata…"
                          : "Rebuild metadata file"
                      }}
                    </span>
                    <span
                      class="text-[9.5px] leading-tight font-normal text-base-content/45"
                    >
                      Write catalog details back to model.json
                    </span>
                  </span>
                </button>
              </li>
              <li v-if="hasAutoSplit">
                <button
                  type="button"
                  class="items-start gap-2 py-2 disabled:opacity-40"
                  :disabled="isFlattening"
                  @click="
                    closeActionMenu($event);
                    flattenGroup();
                  "
                >
                  <span class="mt-0.5 text-base-content/40">↹</span>
                  <span class="flex flex-col items-start">
                    <span class="text-[11px] font-semibold">
                      {{ isFlattening ? "Resetting filing…" : "Reset filing" }}
                    </span>
                    <span
                      class="text-[9.5px] leading-tight font-normal text-base-content/45"
                    >
                      Clear detected variants and poses
                    </span>
                  </span>
                </button>
              </li>
              <li>
                <button
                  type="button"
                  class="items-start gap-2 py-2"
                  @click="
                    closeActionMenu($event);
                    toggleGroupNsfw([
                      selectedGroup?.group_name ?? selected.name,
                    ]);
                  "
                >
                  <span class="mt-0.5">18+</span>
                  <span class="flex flex-col items-start">
                    <span class="text-[11px] font-semibold">
                      {{
                        selectedGroup?.nsfw
                          ? "Remove mature label"
                          : "Mark as mature"
                      }}
                    </span>
                    <span
                      class="text-[9.5px] leading-tight font-normal text-base-content/45"
                    >
                      {{
                        selectedGroup?.nsfw
                          ? "Show while the content filter is locked"
                          : "Hide while the content filter is locked"
                      }}
                    </span>
                  </span>
                </button>
              </li>
              <li class="my-1 h-px bg-base-content/10"></li>
              <li>
                <button
                  type="button"
                  class="items-start gap-2 py-2 text-error hover:bg-error/10!"
                  @click="
                    closeActionMenu($event);
                    openDeleteModal([
                      selectedGroup?.group_name ?? selected.name,
                    ]);
                  "
                >
                  <span class="mt-0.5">⌫</span>
                  <span class="flex flex-col items-start">
                    <span class="text-[11px] font-semibold">Delete model…</span>
                    <span
                      class="text-[9.5px] leading-tight font-normal opacity-60"
                    >
                      Confirmation required · files go to Trash
                    </span>
                  </span>
                </button>
              </li>
            </ul>
          </details>
        </div>

        <div v-if="modelGeometry.length">
          <div
            class="font-mono font-semibold text-[9.5px] tracking-[0.12em] text-base-content/40 mb-1.5"
          >
            GEOMETRY
          </div>
          <div class="font-mono text-[10.5px] text-base-content/60 mb-1.5">
            {{ geometryTallestMm.toFixed(1) }} mm tall ·
            {{ geometryTotalVolumeMl.toFixed(1) }} ml ·
            {{ geometryTotalTriCount.toLocaleString() }} tris
          </div>
          <div
            v-for="g in modelGeometry"
            :key="g.file_name"
            class="flex items-center gap-2 font-mono text-[11px] text-base-content/60 py-0.5"
          >
            <span class="truncate flex-1" :title="g.file_name">{{
              g.file_name
            }}</span>
            <span
              v-if="g.open_edges !== null && g.open_edges > 0"
              class="shrink-0 text-warning"
              :title="`${g.open_edges} open edges — mesh may not be watertight`"
            >
              ⚠
            </span>
            <span
              v-else-if="g.open_edges === null"
              class="shrink-0 opacity-40"
              title="watertightness not checked — this mesh was over the triangle limit when mined; raise the limit in Settings and mine again to backfill"
            >
              ○
            </span>
            <span class="opacity-70 shrink-0">
              {{ g.x_mm.toFixed(1) }}×{{ g.y_mm.toFixed(1) }}×{{
                g.z_mm.toFixed(1)
              }}
              mm
            </span>
            <span class="opacity-60 shrink-0"
              >{{ g.tri_count.toLocaleString() }} tris</span
            >
          </div>
        </div>

        <div>
          <div
            class="flex items-center gap-2 font-mono font-semibold text-[9.5px] tracking-[0.12em] text-base-content/40 mb-1.5"
          >
            <span>FILES · {{ formatFileSize(selected.total_size_bytes) }}</span>
            <span class="flex-1"></span>
            <span class="normal-case tracking-normal font-normal opacity-70">
              tick files to file them under a pose
            </span>
          </div>

          <!-- Assignment bar: always visible on multi-file members.
               Splits a dump folder into pose members without moving
               anything. "match" ticks every file whose name carries
               the typed facets — two spear types x five poses is ten
               type-match-file rounds, not a hundred checkbox clicks. -->
          <div
            v-if="files.length > 1 || checkedFiles.length"
            class="flex items-center gap-1.5 bg-base-200 border border-base-content/10 rounded-lg px-2 py-1.5 mb-1.5"
          >
            <span class="font-mono text-[10px] text-base-content/60 shrink-0">
              {{ checkedFiles.length }} sel
            </span>
            <form
              class="flex items-center gap-1.5 flex-1 min-w-0"
              @submit.prevent="assignChecked"
            >
              <input
                v-model="variantAssignDraft"
                type="text"
                class="input input-xs font-mono flex-1 min-w-0"
                placeholder="variant e.g. sword"
              />
              <input
                v-model="poseAssignDraft"
                type="text"
                class="input input-xs font-mono w-20 shrink-0"
                placeholder="pose"
              />
              <button
                type="button"
                class="btn btn-xs"
                title="Tick every file whose name contains the variant and pose typed above"
                :disabled="
                  !variantAssignDraft.trim() && !poseAssignDraft.trim()
                "
                @click="selectMatchingFiles"
              >
                match
              </button>
              <button
                type="submit"
                class="btn btn-xs btn-primary"
                :disabled="
                  !checkedFiles.length ||
                  (!variantAssignDraft.trim() && !poseAssignDraft.trim())
                "
              >
                file
              </button>
            </form>
            <button
              type="button"
              class="btn btn-xs btn-ghost"
              :disabled="!checkedFiles.length"
              @click="clearChecked"
            >
              unfile
            </button>
          </div>

          <label
            v-for="file in files"
            :key="file.path"
            class="flex items-center gap-2 font-mono text-[11px] text-base-content/60 py-0.5 cursor-pointer"
          >
            <input
              type="checkbox"
              class="checkbox checkbox-xs shrink-0"
              :checked="checkedFiles.includes(file.path)"
              @change="toggleCheckedFile(file.path)"
            />
            <span class="truncate flex-1" :title="file.path">{{
              file.file_name
            }}</span>
            <span
              v-if="fileVariantMap[file.path]"
              class="shrink-0 text-primary"
              title="assigned pose"
            >
              ▸ {{ fileVariantMap[file.path] }}
            </span>
            <span
              v-if="file.packed"
              class="shrink-0"
              title="in the pack archive — extracted on unpack"
            >
              📦
            </span>
            <span class="opacity-60 shrink-0">{{
              formatFileSize(file.size_bytes)
            }}</span>
          </label>
        </div>
      </div>
    </template>
  </aside>
</template>

<script setup lang="ts">
import { defineAsyncComponent } from "vue";
import { convertFileSrc } from "@tauri-apps/api/core";
import { storeToRefs } from "pinia";
// Lazy: StlViewport drags three.js with it, and this component rides the
// eager Catalog boot chunk — the viewport is only needed once a preview
// actually opens (the v-if), not at first paint.
const StlViewport = defineAsyncComponent(() => import("../StlViewport.vue"));
import { useCatalogStore } from "../../stores/catalogStore";
import { useReleasesStore } from "../../stores/releasesStore";
import { formatFileSize } from "../../utils/format";

const store = useCatalogStore();
const releasesStore = useReleasesStore();
const {
  selectedGroup,
  drawerLoadError,
  drawerWidth,
  selected,
  show3d,
  stlPaths,
  viewer3dBusy,
  drawerPreview,
  showImageModal,
  show3dModal,
  isPacking,
  packJobLabel,
  packProgress,
  packedDirs,
  packableDirs,
  renamingGroup,
  groupNameDraft,
  groupSources,
  displayPath,
  measuredLabel,
  modelGeometry,
  geometryTallestMm,
  geometryTotalVolumeMl,
  geometryTotalTriCount,
  supportTabs,
  activeSupport,
  variantsInTab,
  activeVariant,
  tabMembers,
  newTag,
  metaDraft,
  metaDirty,
  structureClean,
  refreshingSidecars,
  hasAutoSplit,
  isFlattening,
  files,
  checkedFiles,
  variantAssignDraft,
  poseAssignDraft,
  fileVariantMap,
} = storeToRefs(store);
const {
  startDrawerResize,
  selectGroup,
  pickPreviewImage,
  useAsCardImage,
  printModel,
  toggle3d,
  renderSelected,
  cancelPack,
  packSelectedGroup,
  unpackSelectedGroup,
  renameGroup,
  startRenameGroup,
  splitGroup,
  detachSelectedSource,
  reveal,
  setSupportTab,
  tabLabel,
  setVariant,
  variantLabel,
  memberKey,
  selectEntry,
  addTag,
  removeTag,
  saveMetadata,
  refreshSidecars,
  cleanUpSelectedGroup,
  addToDraftRelease,
  flattenGroup,
  openDeleteModal,
  toggleGroupNsfw,
  selectMatchingFiles,
  assignChecked,
  clearChecked,
  toggleCheckedFile,
} = store;

/** Native details gives the action menu keyboard semantics without another
 * overlay dependency; action buttons close it explicitly after selection. */
const closeActionMenu = (event: MouseEvent) => {
  (event.currentTarget as HTMLElement)
    .closest("details")
    ?.removeAttribute("open");
};
</script>
