<template>
  <main class="h-full overflow-y-auto p-7">
    <div class="max-w-150 flex flex-col gap-4">
      <div class="font-bold text-[17px]">Settings</div>

      <div class="flex flex-col gap-1.5">
        <span
          class="font-mono font-semibold text-[10px] tracking-widest text-base-content/40"
          >CATALOG FOLDERS</span
        >
        <div
          class="flex flex-col gap-1 bg-base-200 border border-base-content/10 rounded-lg px-2.5 py-1.5"
        >
          <span
            v-for="root in catalogRoots"
            :key="root"
            class="font-mono text-[12px] text-base-content/60 truncate"
            :title="root"
            >{{ root
            }}<span
              v-if="root === settings.catalog_primary_root"
              class="text-warning"
              title="Primary — Clean up moves every folder's models into this one"
            >
              ★ primary</span
            ></span
          >
          <span
            v-if="!catalogRoots.length"
            class="font-mono text-[12px] text-base-content/40"
            >No folders yet</span
          >
        </div>
        <span class="text-[10.5px] text-base-content/40"
          >Add, scan, and remove folders from the Catalog tab — one designer
          folder at a time works best for huge collections.</span
        >
      </div>

      <div v-if="ignoredFolders.length" class="flex flex-col gap-1.5">
        <span
          class="font-mono font-semibold text-[10px] tracking-widest text-base-content/40"
          >HIDDEN FROM THE CATALOG</span
        >
        <div
          class="flex flex-col gap-1 bg-base-200 border border-base-content/10 rounded-lg px-2.5 py-1.5"
        >
          <div
            v-for="folder in ignoredFolders"
            :key="folder.dir_path"
            class="flex items-center gap-2"
          >
            <span
              class="font-mono text-[12px] text-base-content/60 flex-1 truncate"
              :title="folder.dir_path"
              >{{ folder.dir_path }}</span
            >
            <button
              type="button"
              class="btn btn-xs btn-ghost"
              title="Let scans see this folder again — it reappears after the next scan of its catalog folder"
              @click="unignoreFolder(folder.dir_path)"
            >
              unhide
            </button>
          </div>
        </div>
        <span class="text-[10.5px] text-base-content/40"
          >Removed from the catalog but still on disk. Scans skip these folders;
          unhide one and rescan to bring it back.</span
        >
      </div>

      <div class="flex flex-col gap-1.5">
        <span
          class="font-mono font-semibold text-[10px] tracking-widest text-base-content/40"
          >MATURE CONTENT</span
        >
        <div
          class="flex flex-col gap-2 bg-base-200 border border-base-content/10 rounded-lg px-2.5 py-1.5"
        >
          <label class="flex items-center gap-2 cursor-pointer w-fit">
            <input
              type="checkbox"
              class="toggle toggle-sm"
              :checked="nsfwAccess.unlocked"
              @change="onToggleShowNsfw"
            />
            <span class="font-mono text-[12px] text-base-content/70"
              >Show 18+ models</span
            >
          </label>

          <form
            v-if="showUnlockPrompt"
            class="flex items-center gap-1.5"
            @submit.prevent="confirmUnlock"
          >
            <input
              v-model="unlockPinInput"
              type="password"
              inputmode="numeric"
              class="input input-xs font-mono w-24"
              placeholder="PIN"
              autofocus
            />
            <button type="submit" class="btn btn-xs btn-primary">Unlock</button>
            <button
              type="button"
              class="btn btn-xs btn-ghost"
              @click="cancelUnlock"
            >
              cancel
            </button>
            <button
              v-if="nsfwAccess.recovery_configured"
              type="button"
              class="btn btn-xs btn-ghost"
              @click="startRecovery"
            >
              forgot PIN?
            </button>
          </form>

          <form
            v-if="showRecovery"
            class="flex flex-wrap items-center gap-1.5"
            @submit.prevent="confirmRecovery"
          >
            <input
              v-model="recoveryCodeInput"
              type="text"
              class="input input-xs font-mono w-72"
              placeholder="recovery code"
            />
            <input
              v-model="recoveryNewPin"
              type="password"
              inputmode="numeric"
              class="input input-xs font-mono w-24"
              placeholder="new PIN"
            />
            <button type="submit" class="btn btn-xs btn-primary">
              Reset PIN
            </button>
            <button
              type="button"
              class="btn btn-xs btn-ghost"
              @click="cancelRecovery"
            >
              cancel
            </button>
          </form>

          <div
            v-if="issuedRecoveryCode"
            class="rounded-md border border-warning/40 bg-warning/10 p-2"
          >
            <div class="font-mono text-[11px] text-base-content/70">
              Save this recovery code outside this computer. It is shown only
              once.
            </div>
            <div class="font-mono text-[13px] font-semibold select-all my-1">
              {{ issuedRecoveryCode }}
            </div>
            <button
              type="button"
              class="btn btn-xs"
              @click="issuedRecoveryCode = ''"
            >
              I saved it
            </button>
          </div>

          <div class="h-px bg-base-content/10"></div>

          <template v-if="!nsfwAccess.pin_configured">
            <form class="flex items-center gap-1.5" @submit.prevent="setPin">
              <span
                class="font-mono text-[11px] text-base-content/50 w-24 shrink-0"
                >Lock behind a PIN</span
              >
              <input
                v-model="newPinInput"
                type="password"
                inputmode="numeric"
                class="input input-xs font-mono w-24"
                placeholder="4–12 digits"
              />
              <button type="submit" class="btn btn-xs" :disabled="!newPinInput">
                Set PIN
              </button>
            </form>
          </template>
          <template v-else>
            <div class="flex items-center gap-1.5">
              <span class="font-mono text-[11px] text-base-content/50 flex-1"
                >PIN set — required to turn the toggle on</span
              >
              <button
                type="button"
                class="btn btn-xs btn-ghost"
                @click="startChangePin"
              >
                Change PIN…
              </button>
              <button
                type="button"
                class="btn btn-xs btn-ghost"
                @click="startRemovePin"
              >
                Remove PIN
              </button>
            </div>
            <form
              v-if="showChangePin"
              class="flex items-center gap-1.5"
              @submit.prevent="confirmChangePin"
            >
              <input
                v-model="changeCurrentPin"
                type="password"
                inputmode="numeric"
                class="input input-xs font-mono w-20"
                placeholder="current"
              />
              <input
                v-model="changeNewPin"
                type="password"
                inputmode="numeric"
                class="input input-xs font-mono w-20"
                placeholder="new"
              />
              <button
                type="submit"
                class="btn btn-xs btn-primary"
                :disabled="!changeCurrentPin || !changeNewPin"
              >
                save
              </button>
              <button
                type="button"
                class="btn btn-xs btn-ghost"
                @click="showChangePin = false"
              >
                cancel
              </button>
            </form>
            <form
              v-if="showRemovePin"
              class="flex items-center gap-1.5"
              @submit.prevent="confirmRemovePin"
            >
              <input
                v-model="removePinInput"
                type="password"
                inputmode="numeric"
                class="input input-xs font-mono w-24"
                placeholder="current PIN"
              />
              <button type="submit" class="btn btn-xs btn-error">remove</button>
              <button
                type="button"
                class="btn btn-xs btn-ghost"
                @click="showRemovePin = false"
              >
                cancel
              </button>
            </form>
          </template>

          <div class="h-px bg-base-content/10"></div>

          <div
            v-if="nsfwAccess.unlocked"
            class="flex flex-wrap gap-1.5 items-center"
          >
            <span
              v-for="designer in nsfwDesigners"
              :key="designer"
              class="font-mono text-[11px] text-base-content/70 border border-base-content/15 rounded-full px-2.5 py-0.5 flex items-center gap-1"
            >
              {{ designer }}
              <button
                type="button"
                class="opacity-50 hover:opacity-100"
                @click="removeNsfwDesigner(designer)"
              >
                ✕
              </button>
            </span>
            <form class="join" @submit.prevent="addNsfwDesigner">
              <input
                v-model="newNsfwDesigner"
                type="text"
                class="input input-xs join-item w-40 font-mono"
                placeholder="+ designer, all 18+"
              />
            </form>
          </div>
          <span v-else class="font-mono text-[11px] text-base-content/45">
            Unlock to view or change mature-content designer rules. You can
            still mark a visible catalog model as 18+ while locked.
          </span>
        </div>
        <p class="text-[10.5px] text-base-content/40">
          Models marked 18+ — and everything by the designers listed here — are
          hidden from browsing while locked. Access relocks whenever Plinth
          restarts. Scanning still indexes them and the files remain on disk;
          this is a family-PC filter, not encryption or OS parental controls.
        </p>
      </div>

      <div class="flex flex-col gap-1.5">
        <FileSelect
          id="scratch_dir"
          label="Temporary files directory"
          dir-mode
          v-model="settings.scratch_dir"
          tooltip="Your files will be temporarily stored here before being compressed."
        />
        <span class="text-[10.5px] text-base-content/40"
          >Files are staged here before compression.</span
        >
      </div>

      <div class="flex flex-col gap-1.5">
        <FileSelect
          id="target_dir"
          label="Target directory — finished .3pk releases"
          dir-mode
          v-model="settings.target_dir"
          tooltip="Your compressed files will be saved here."
        />
      </div>

      <div class="flex flex-col gap-1.5">
        <span
          class="font-mono font-semibold text-[10px] tracking-widest text-base-content/40"
          >BLENDER LOCATION</span
        >
        <div
          class="flex items-center gap-2.5 bg-base-200 border border-base-content/10 rounded-lg px-2.5 py-1.5"
        >
          <span
            class="font-mono text-[12px] text-base-content/60 flex-1 truncate"
          >
            {{
              settings.blender_path ||
              "Auto-detect (PATH, /Applications, BLENDER_BIN)"
            }}
          </span>
          <button type="button" class="btn btn-xs" @click="browseBlender">
            Browse…
          </button>
          <button type="button" class="btn btn-xs" @click="checkBlender">
            Detect
          </button>
          <button
            v-if="verdict && verdict !== 'Ok' && !isDownloading"
            type="button"
            class="btn btn-xs btn-primary"
            @click="startDownload"
          >
            Download {{ managedVersion }}
          </button>
        </div>
        <div v-if="isDownloading" class="flex items-center gap-2">
          <progress
            class="progress progress-primary flex-1"
            :value="downloadPercent"
            max="100"
          ></progress>
          <span class="font-mono text-[10px] text-base-content/50">{{
            downloadPhase ? `${downloadPhase}…` : `${downloadPercent}%`
          }}</span>
        </div>
        <p
          v-if="blenderStatusText"
          class="text-[10.5px] font-mono"
          :class="blenderStatusClass"
        >
          {{ blenderStatusText }}
        </p>
        <p v-if="downloadError" class="text-[10.5px] font-mono text-error">
          {{ downloadError }}
        </p>
      </div>

      <div class="flex flex-col gap-1.5">
        <span
          class="font-mono font-semibold text-[10px] tracking-widest text-base-content/40"
          >MAX COMPRESSION THREADS — {{ availableCores }} CORES DETECTED</span
        >
        <div class="flex items-center gap-3">
          <input
            id="max_compression_threads"
            type="range"
            min="1"
            :max="availableCores"
            v-model.number="settings.max_compression_threads"
            class="range range-primary range-sm flex-1"
          />
          <span class="font-mono font-semibold text-[13px] w-6 text-right">
            {{ settings.max_compression_threads || "Auto" }}
          </span>
        </div>
        <p class="text-[10.5px] text-base-content/40">
          Lower for better system responsiveness, higher for faster compression.
          Default is automatically calculated ({{ defaultThreadCount }}).
        </p>
      </div>

      <div class="flex flex-col gap-1.5">
        <span
          class="font-mono font-semibold text-[10px] tracking-widest text-base-content/40"
          >WATERTIGHTNESS CHECK LIMIT — TRIANGLES PER MESH</span
        >
        <div class="flex items-center gap-3">
          <input
            id="edge_stats_max_tris"
            type="range"
            min="1500000"
            max="30000000"
            step="100000"
            v-model.number="settings.edge_stats_max_tris"
            class="range range-primary range-sm flex-1"
          />
          <span class="font-mono font-semibold text-[13px] w-20 text-right">
            {{ edgeCapLabel }}
          </span>
          <button type="button" class="btn btn-xs" @click="resetEdgeCapToAuto">
            auto
          </button>
        </div>
        <p class="text-[10.5px] text-base-content/40">
          Meshes above this triangle count still get dimensions, volume, and
          triangle counts when mining geometry — only the open-edge (watertight)
          check is skipped, and the drawer marks it unchecked. The check costs
          up to ~150 bytes of memory per triangle while a file is being mined
          ({{ edgeCapMemoryLabel }} peak at the current limit). "auto" restores
          the value recommended for this machine's RAM. Raising the limit and
          mining again backfills meshes that were skipped under a lower one.
        </p>
      </div>

      <div class="flex flex-col gap-1.5">
        <span
          class="font-mono font-semibold text-[10px] tracking-widest text-base-content/40"
          >KNOWN DESIGNERS — RECOGNIZED IN FOLDER NAMES WHEN SCANNING</span
        >
        <div
          class="flex flex-wrap gap-1.5 items-center bg-base-200 border border-base-content/10 rounded-lg p-2"
        >
          <span
            v-for="designer in settings.known_designers ?? []"
            :key="designer"
            class="font-mono text-[11px] text-base-content/70 border border-base-content/15 rounded-full px-2.5 py-0.5 flex items-center gap-1"
          >
            {{ designer }}
            <button
              type="button"
              class="opacity-50 hover:opacity-100"
              @click="removeDesigner(designer)"
            >
              ✕
            </button>
          </span>
          <form class="join" @submit.prevent="addDesigner">
            <input
              v-model="newDesigner"
              type="text"
              class="input input-xs join-item w-40 font-mono"
              placeholder="+ add designer"
            />
          </form>
        </div>
        <p class="text-[10.5px] text-base-content/40">
          Infers a model's designer from its folder path when there's no release
          metadata. Matching ignores case, spaces and punctuation. Applies on
          the next scan.
        </p>
      </div>

      <div class="flex flex-col gap-1.5">
        <span
          class="font-mono font-semibold text-[10px] tracking-widest text-base-content/40"
          >MAGNET INVENTORY — DIAMETER × HEIGHT (MM)</span
        >
        <div
          class="flex flex-wrap gap-1.5 items-center bg-base-200 border border-base-content/10 rounded-lg p-2"
        >
          <span
            v-for="(magnet, index) in settings.magnet_inventory ?? []"
            :key="`${magnet.diameter_mm}x${magnet.height_mm}-${index}`"
            class="font-mono text-[11px] text-base-content/70 border border-base-content/15 rounded-full px-2.5 py-0.5 flex items-center gap-1"
          >
            {{ magnet.diameter_mm }}×{{ magnet.height_mm }}
            <button
              type="button"
              class="opacity-50 hover:opacity-100"
              @click="removeMagnet(index)"
            >
              ✕
            </button>
          </span>
          <form class="join" @submit.prevent="addMagnet">
            <input
              v-model.number="newMagnetDiameter"
              type="number"
              step="0.5"
              min="0.5"
              class="input input-xs join-item w-14 font-mono"
              placeholder="⌀"
            />
            <input
              v-model.number="newMagnetHeight"
              type="number"
              step="0.5"
              min="0.5"
              class="input input-xs join-item w-14 font-mono"
              placeholder="h"
            />
            <button type="submit" class="btn btn-xs join-item">+ add</button>
          </form>
        </div>
        <p class="text-[10.5px] text-base-content/40">
          Magnets you actually own. Base Cutter's per-placement magnet panel
          offers one chip per size here and suggests the largest one whose boss
          fits the base — always overridable per placement.
        </p>
      </div>

      <div class="flex flex-col gap-1.5">
        <span
          class="font-mono font-semibold text-[10px] tracking-widest text-base-content/40"
          >SCATTER LIBRARY</span
        >
        <div
          class="flex items-center gap-2.5 bg-base-200 border border-base-content/10 rounded-lg px-2.5 py-1.5"
        >
          <span
            class="font-mono text-[12px] text-base-content/60 flex-1 truncate"
          >
            {{ settings.scatter_library_dir || "No folder configured" }}
          </span>
          <button
            type="button"
            class="btn btn-xs"
            @click="browseScatterLibrary"
          >
            Browse…
          </button>
          <button
            v-if="settings.scatter_library_dir"
            type="button"
            class="btn btn-xs btn-ghost"
            @click="settings.scatter_library_dir = null"
          >
            clear
          </button>
        </div>
        <p class="text-[10.5px] text-base-content/40">
          A flat folder of your own scatter pieces (*.stl) — Base Cutter's piece
          mix picker offers them alongside the bundled set, right next to a
          "rescan" button. Non-recursive; scanned fresh every time.
        </p>

        <details
          class="collapse collapse-arrow border border-base-content/10 bg-base-200/20 rounded-box"
        >
          <summary
            class="collapse-title min-h-0 py-2 px-3 flex items-center gap-2 cursor-pointer"
            @toggle="loadScatterCredits"
          >
            <span
              class="font-mono font-semibold text-[10px] tracking-widest text-base-content/40"
              >CREDITS — BUNDLED SCATTER PIECES</span
            >
          </summary>
          <div class="collapse-content px-3">
            <p
              v-if="scatterCreditsLoading"
              class="text-[10.5px] text-base-content/40"
            >
              Loading…
            </p>
            <pre
              v-else
              class="text-[10.5px] whitespace-pre-wrap font-mono text-base-content/70"
              >{{ scatterCredits }}</pre
            >
          </div>
        </details>
      </div>

      <div class="flex flex-col gap-1.5">
        <span
          class="font-mono font-semibold text-[10px] tracking-widest text-base-content/40"
          >PRINT BUTTON</span
        >
        <div
          class="flex gap-1 bg-base-200 border border-base-content/10 rounded-full p-0.75 w-55"
        >
          <button
            type="button"
            class="flex-1 text-center font-semibold text-[11px] py-1.5 rounded-full cursor-pointer"
            :class="
              printAction === 'open-in-slicer'
                ? 'bg-primary text-primary-content'
                : 'text-base-content/60'
            "
            @click="settings.print_action = 'open-in-slicer'"
          >
            Open in slicer
          </button>
          <button
            type="button"
            class="flex-1 text-center font-semibold text-[11px] py-1.5 rounded-full cursor-pointer"
            :class="
              printAction === 'reveal-folder'
                ? 'bg-primary text-primary-content'
                : 'text-base-content/60'
            "
            @click="settings.print_action = 'reveal-folder'"
          >
            Reveal folder
          </button>
        </div>
        <p class="text-[10.5px] text-base-content/40">
          Open in slicer lets you tick which of the model's files to send to
          whatever app your system opens them with (pre-sliced scenes are
          pre-selected). Reveal folder shows them in Finder/Explorer instead —
          handy if you switch between slicers.
        </p>
      </div>

      <div class="flex flex-col gap-1.5">
        <span
          class="font-mono font-semibold text-[10px] tracking-widest text-base-content/40"
          >PACKED MODELS</span
        >
        <div
          class="flex gap-1 bg-base-200 border border-base-content/10 rounded-full p-0.75 w-55"
        >
          <button
            type="button"
            class="flex-1 text-center font-semibold text-[11px] py-1.5 rounded-full cursor-pointer"
            :class="
              packCleanup
                ? 'bg-primary text-primary-content'
                : 'text-base-content/60'
            "
            @click="settings.pack_cleanup_after = true"
          >
            Clean up after use
          </button>
          <button
            type="button"
            class="flex-1 text-center font-semibold text-[11px] py-1.5 rounded-full cursor-pointer"
            :class="
              !packCleanup
                ? 'bg-primary text-primary-content'
                : 'text-base-content/60'
            "
            @click="settings.pack_cleanup_after = false"
          >
            Keep extracted
          </button>
        </div>
        <label class="flex items-center gap-2 text-[11px]">
          <span class="text-base-content/60">Compression level</span>
          <input
            :value="settings.pack_level ?? 3"
            type="number"
            min="1"
            max="19"
            class="input input-xs w-16 font-mono"
            @change="
              settings.pack_level =
                Number.parseInt(
                  ($event.target as HTMLInputElement).value,
                  10,
                ) || null
            "
          />
          <span class="text-base-content/40">zstd, default 3</span>
        </label>
        <p class="text-[10.5px] text-base-content/40">
          Printing or previewing a packed model extracts just the needed files
          from its archive. Clean up after use removes those temporary copies
          again once the action is done — the closest thing to printing straight
          from the bundle. Higher compression levels pack smaller but slower;
          extraction speed is unaffected.
        </p>
      </div>

      <div class="flex flex-col gap-1.5">
        <span
          class="font-mono font-semibold text-[10px] tracking-widest text-base-content/40"
          >SCALE FIGURE</span
        >
        <div
          class="flex items-center gap-2.5 bg-base-200 border border-base-content/10 rounded-lg px-2.5 py-1.5"
        >
          <span
            class="font-mono text-[12px] text-base-content/60 flex-1 truncate"
          >
            {{ settings.scale_reference_path || "No figure chosen" }}
          </span>
          <button type="button" class="btn btn-xs" @click="browseScaleRef">
            Browse…
          </button>
          <button
            v-if="settings.scale_reference_path"
            type="button"
            class="btn btn-xs btn-ghost"
            @click="settings.scale_reference_path = null"
          >
            clear
          </button>
        </div>
        <label class="flex items-center gap-2 text-[11px]">
          <span class="text-base-content/60">Stands</span>
          <input
            :value="settings.scale_reference_height_mm ?? 28"
            type="number"
            min="1"
            max="500"
            step="0.5"
            class="input input-xs w-16 font-mono"
            @change="
              settings.scale_reference_height_mm =
                Number.parseFloat(($event.target as HTMLInputElement).value) ||
                null
            "
          />
          <span class="text-base-content/40">mm tall next to the model</span>
        </label>
        <p class="text-[10.5px] text-base-content/40">
          A reference figure rendered in grey beside your model at true relative
          size — the "banana for scale". Any STL works (a 28&nbsp;mm standing
          person reads best); toggle it per render in the studio.
        </p>
      </div>

      <div class="flex flex-col gap-1.5">
        <span
          class="font-mono font-semibold text-[10px] tracking-widest text-base-content/40"
          >CREATOR LICENCE</span
        >
        <div
          class="flex items-center gap-2.5 bg-base-200 border border-base-content/10 rounded-lg px-2.5 py-1.5"
        >
          <span
            class="font-mono text-[12px] text-base-content/60 flex-1 truncate"
          >
            {{ settings.licence_path || "No licence file chosen" }}
          </span>
          <button type="button" class="btn btn-xs" @click="browseLicence">
            Browse…
          </button>
          <button
            v-if="settings.licence_path"
            type="button"
            class="btn btn-xs btn-ghost"
            @click="settings.licence_path = null"
          >
            clear
          </button>
        </div>
        <p class="text-[10.5px] text-base-content/40">
          Your licence terms as a file (PDF, txt, md…). The release builder
          offers to include it in every release you pack — it travels inside the
          release.3pk, named licence, so your customers always receive your
          terms alongside the models.
        </p>
      </div>

      <div class="flex flex-col gap-1.5">
        <span
          class="font-mono font-semibold text-[10px] tracking-widest text-base-content/40"
          >APPEARANCE</span
        >
        <div
          class="flex gap-1 bg-base-200 border border-base-content/10 rounded-full p-0.75 w-55"
        >
          <button
            type="button"
            class="flex-1 text-center font-semibold text-[11px] py-1.5 rounded-full cursor-pointer"
            :class="
              themeStore.isDark()
                ? 'bg-primary text-primary-content'
                : 'text-base-content/60'
            "
            @click="themeStore.setDark"
          >
            Dark
          </button>
          <button
            type="button"
            class="flex-1 text-center font-semibold text-[11px] py-1.5 rounded-full cursor-pointer"
            :class="
              !themeStore.isDark()
                ? 'bg-primary text-primary-content'
                : 'text-base-content/60'
            "
            @click="themeStore.setLight"
          >
            Light
          </button>
        </div>
      </div>
    </div>
  </main>
</template>

<script setup lang="ts">
import { computed, onActivated, onMounted, ref, watch } from "vue";
import {
  type IgnoredFolder,
  type NsfwAccessState,
  type Settings,
  commands,
} from "../bindings.ts";
import FileSelect from "../components/FileSelect.vue";
import { useBlenderProvision } from "../composables/useBlenderProvision";
import { useFileSelect } from "../composables/useFileSelect";
import { useThemeStore } from "../stores/themeStore";
import { useToastStore } from "../stores/toastStore";

const toastStore = useToastStore();
const { selectFiles, selectDirectory } = useFileSelect();
const themeStore = useThemeStore();

const settings = ref<Settings>({
  scratch_dir: null,
  target_dir: null,
  compression_type: "Zip",
  chunk_size: null,
  max_compression_threads: null,
  blender_path: null,
  catalog_root: null,
  catalog_roots: null,
  catalog_primary_root: null,
  known_designers: null,
  print_action: null,
  release_field_defaults: null,
  pack_level: null,
  pack_cleanup_after: null,
  blender_setup_acknowledged: null,
  scatter_library_dir: null,
  edge_stats_max_tris: null,
});

// Display only — the Catalog tab manages the list. Falls back to the
// legacy single root for a store that predates multi-root.
const catalogRoots = computed(
  () =>
    settings.value.catalog_roots ??
    (settings.value.catalog_root ? [settings.value.catalog_root] : []),
);

// Unset means the default behavior: hand files straight to the slicer
const printAction = computed(
  () => settings.value.print_action ?? "open-in-slicer",
);

// Unset means the default: extracted working copies are taken back after use
const packCleanup = computed(() => settings.value.pack_cleanup_after ?? true);

/* Both chip-list editors below (designer lexicon, magnet inventory) share
   one add rule: append unless a duplicate already exists, where each list
   defines its own duplicate key. One helper instead of two hand-rolled
   copies, so a future editing rule (validation, undo) lands in one place. */
const addUnique = <T>(
  list: T[],
  item: T,
  isDuplicate: (existing: T) => boolean,
): T[] => (list.some(isDuplicate) ? list : [...list, item]);

/* The scanner's designer lexicon, editable here; seeded server-side with
   sensible defaults. Mutating the array triggers the deep-watch auto-save. */
const newDesigner = ref("");
const addDesigner = () => {
  const name = newDesigner.value.trim();
  newDesigner.value = "";
  if (!name) return;
  settings.value.known_designers = addUnique(
    settings.value.known_designers ?? [],
    name,
    (d) => d.toLowerCase() === name.toLowerCase(),
  );
};
const removeDesigner = (name: string) => {
  settings.value.known_designers = (
    settings.value.known_designers ?? []
  ).filter((d) => d !== name);
};

/* The magnet inventory (docs/BASECUTTER.md "Hollow, with magnet mounts"),
   editable here; seeded server-side with the common hobby sizes. Base
   Cutter's magnet panel reads this list and never writes it back. */
const newMagnetDiameter = ref<number | null>(null);
const newMagnetHeight = ref<number | null>(null);
const addMagnet = () => {
  const diameter_mm = newMagnetDiameter.value;
  const height_mm = newMagnetHeight.value;
  newMagnetDiameter.value = null;
  newMagnetHeight.value = null;
  if (!diameter_mm || !height_mm || diameter_mm <= 0 || height_mm <= 0) return;
  settings.value.magnet_inventory = addUnique(
    settings.value.magnet_inventory ?? [],
    { diameter_mm, height_mm, count: 1 },
    (m) => m.diameter_mm === diameter_mm && m.height_mm === height_mm,
  );
};
const removeMagnet = (index: number) => {
  settings.value.magnet_inventory = (
    settings.value.magnet_inventory ?? []
  ).filter((_, i) => i !== index);
};

// Shared with the first-run dialog and the Render tab — one verdict, three
// surfaces. The status line derives from it, so a download finishing (even
// one started elsewhere) updates this tab without a manual re-detect.
const {
  check: blenderCheck,
  checking: blenderChecking,
  verdict,
  managedVersion,
  runCheck,
  isDownloading,
  percent: downloadPercent,
  phase: downloadPhase,
  errorMessage: downloadError,
  startDownload,
} = useBlenderProvision();

const browseBlender = async () => {
  const files = await selectFiles({
    multiple: false,
    title: "Select Blender executable",
  });
  if (files?.length) {
    settings.value.blender_path = files[0].path;
    await checkBlender();
  }
};

const browseScaleRef = async () => {
  const files = await selectFiles({
    multiple: false,
    title: "Select the scale figure STL",
    accept: ".stl",
  });
  if (files?.length) {
    settings.value.scale_reference_path = files[0].path;
  }
};

const browseLicence = async () => {
  const files = await selectFiles({
    multiple: false,
    title: "Select your licence file",
  });
  if (files?.length) {
    settings.value.licence_path = files[0].path;
  }
};

/* Scatter library (docs/SCATTER.md "User library") — Base Cutter's own
   piece-mix picker only reads settings.scatter_library_dir and scans it
   itself; this view just points it at a folder (or clears it). */
const browseScatterLibrary = async () => {
  const dir = await selectDirectory({ title: "Select scatter library folder" });
  if (dir) settings.value.scatter_library_dir = dir;
};

/* Bundled scatter pieces are all CC0 (nothing legally owed), but
   get_scatter_credits() still lists the source institutions — fetched
   lazily on first expand rather than on every Settings load, since most
   visits never open this disclosure. */
const scatterCredits = ref("");
const scatterCreditsLoading = ref(false);
const scatterCreditsLoaded = ref(false);
const loadScatterCredits = async () => {
  if (scatterCreditsLoaded.value || scatterCreditsLoading.value) return;
  scatterCreditsLoading.value = true;
  try {
    scatterCredits.value = await commands.getScatterCredits();
    scatterCreditsLoaded.value = true;
  } catch (error) {
    toastStore.reportError("Failed to load scatter credits", error);
  } finally {
    scatterCreditsLoading.value = false;
  }
};

const checkBlender = async () => {
  await runCheck();
};

const blenderStatusText = computed(() => {
  if (blenderChecking.value) return "Checking...";
  const check = blenderCheck.value;
  if (!check) return "";
  const managed = check.is_managed ? " (managed by Plinth)" : "";
  if (!check.info)
    return "Blender not found. Download it here or point to an install.";
  switch (check.verdict) {
    case "Outdated":
      return `△ ${check.info.version} works, but previews are tuned for Blender ${check.managed_version}`;
    case "TooOld":
      return `✗ ${check.info.version} is below the 4.2 minimum — rendering is disabled`;
    default:
      return `✓ Found ${check.info.version} at ${check.info.path}${managed}`;
  }
});

const blenderStatusClass = computed(() => {
  switch (verdict.value) {
    case "Ok":
      return "text-success";
    case "Outdated":
      return "text-warning";
    default:
      return "text-error";
  }
});

const availableCores = ref(navigator.hardwareConcurrency || 4);
const defaultThreadCount = computed(() =>
  Math.max(1, availableCores.value - 1),
);

// Mirrors the backend's floor (stl_facts::EDGE_STATS_MAX_TRIS) — the store
// clamps on load/save anyway; this is just the pre-load display fallback.
const EDGE_CAP_FLOOR = 1_500_000;
const edgeCapLabel = computed(() => {
  const cap = settings.value.edge_stats_max_tris ?? EDGE_CAP_FLOOR;
  return `${(cap / 1_000_000).toFixed(1)}M tris`;
});
// The honest cost readout: worst-case ~150 bytes of transient memory per
// triangle while one file is being mined (same constant the backend's
// recommendation formula uses).
const edgeCapMemoryLabel = computed(() => {
  const bytes = (settings.value.edge_stats_max_tris ?? EDGE_CAP_FLOOR) * 150;
  const gib = bytes / (1024 * 1024 * 1024);
  return gib >= 1
    ? `≈${gib.toFixed(1)} GB`
    : `≈${Math.round(bytes / (1024 * 1024))} MB`;
});
const resetEdgeCapToAuto = async () => {
  const recommended = await commands.getRecommendedEdgeCap();
  if (recommended.status === "ok") {
    settings.value.edge_stats_max_tris = recommended.data;
  } else {
    toastStore.reportError(
      "Could not compute the recommended limit",
      recommended.error,
    );
  }
};

let saveTimeout: number | null = null;
const debouncedSave = () => {
  if (saveTimeout) clearTimeout(saveTimeout);
  saveTimeout = setTimeout(() => {
    saveSettings();
  }, 500) as unknown as number;
};

// Watch the settings object itself instead of relying on native form
// events: the directory pickers update the model via a Vue emit, which
// fires no DOM change/blur event, so form listeners never saw them.
const settingsLoaded = ref(false);
watch(
  settings,
  () => {
    if (settingsLoaded.value) debouncedSave();
  },
  { deep: true },
);

/* Soft-removed models ("remove from catalog, keep the files"): scans skip
   them until unhidden here. Unhiding only drops the marker — the models
   reappear once their catalog folder is rescanned. */
const ignoredFolders = ref<IgnoredFolder[]>([]);
const loadIgnoredFolders = async () => {
  const result = await commands.listIgnoredFolders();
  if (result.status === "ok") ignoredFolders.value = result.data;
};
const unignoreFolder = async (dirPath: string) => {
  const result = await commands.unignoreFolder(dirPath);
  if (result.status !== "ok") {
    toastStore.reportError("Failed to unhide folder", result.error);
    return;
  }
  toastStore.addToast(
    "Folder unhidden — rescan its catalog folder to bring the models back",
    "success",
  );
  await loadIgnoredFolders();
};

/* ---- mature content: backend-owned, session-only family-PC filter ---- */

const nsfwAccess = ref<NsfwAccessState>({
  unlocked: false,
  pin_configured: false,
  recovery_configured: false,
});

const syncNsfwAccess = async () => {
  const result = await commands.getNsfwAccessState();
  if (result.status === "ok") nsfwAccess.value = result.data;
};

const showUnlockPrompt = ref(false);
const unlockPinInput = ref("");
const onToggleShowNsfw = async (event: Event) => {
  const wantsOn = (event.target as HTMLInputElement).checked;
  if (!wantsOn) {
    const result = await commands.lockNsfw();
    if (result.status === "ok") {
      nsfwAccess.value = result.data;
      nsfwDesigners.value = [];
    } else {
      toastStore.reportError("Failed to lock mature content", result.error);
    }
    return;
  }
  if (!nsfwAccess.value.pin_configured) {
    const result = await commands.unlockNsfw(null);
    if (result.status === "ok") {
      nsfwAccess.value = result.data;
      await loadNsfwDesigners();
    } else {
      toastStore.reportError("Failed to show mature content", result.error);
    }
    return;
  }
  showUnlockPrompt.value = true;
  unlockPinInput.value = "";
};
const confirmUnlock = async () => {
  const result = await commands.unlockNsfw(unlockPinInput.value);
  if (result.status !== "ok") {
    toastStore.reportError("Could not unlock mature content", result.error);
    return;
  }
  nsfwAccess.value = result.data;
  showUnlockPrompt.value = false;
  unlockPinInput.value = "";
  await loadNsfwDesigners();
};
const cancelUnlock = () => {
  showUnlockPrompt.value = false;
  unlockPinInput.value = "";
};

const issuedRecoveryCode = ref("");
const newPinInput = ref("");
const setPin = async () => {
  if (!newPinInput.value) return;
  const result = await commands.configureNsfwPin(newPinInput.value);
  if (result.status !== "ok") {
    toastStore.reportError("Failed to set PIN", result.error);
    return;
  }
  nsfwAccess.value = result.data.state;
  issuedRecoveryCode.value = result.data.recovery_code;
  newPinInput.value = "";
  toastStore.addToast("PIN set — save the recovery code", "success");
};

const showChangePin = ref(false);
const changeCurrentPin = ref("");
const changeNewPin = ref("");
const startChangePin = () => {
  showChangePin.value = true;
  showRemovePin.value = false;
  changeCurrentPin.value = "";
  changeNewPin.value = "";
};
const confirmChangePin = async () => {
  const result = await commands.changeNsfwPin(
    changeCurrentPin.value,
    changeNewPin.value,
  );
  if (result.status !== "ok") {
    toastStore.reportError("Failed to change PIN", result.error);
    return;
  }
  nsfwAccess.value = result.data;
  showChangePin.value = false;
  changeCurrentPin.value = "";
  changeNewPin.value = "";
  toastStore.addToast("PIN changed", "success");
};

const showRemovePin = ref(false);
const removePinInput = ref("");
const startRemovePin = () => {
  showRemovePin.value = true;
  showChangePin.value = false;
  removePinInput.value = "";
};
const confirmRemovePin = async () => {
  const result = await commands.removeNsfwPin(removePinInput.value);
  if (result.status !== "ok") {
    toastStore.reportError("Failed to remove PIN", result.error);
    return;
  }
  nsfwAccess.value = result.data;
  nsfwDesigners.value = [];
  showRemovePin.value = false;
  removePinInput.value = "";
  toastStore.addToast("PIN removed", "success");
};

const showRecovery = ref(false);
const recoveryCodeInput = ref("");
const recoveryNewPin = ref("");
const startRecovery = () => {
  showUnlockPrompt.value = false;
  showRecovery.value = true;
  recoveryCodeInput.value = "";
  recoveryNewPin.value = "";
};
const cancelRecovery = () => {
  showRecovery.value = false;
  recoveryCodeInput.value = "";
  recoveryNewPin.value = "";
};
const confirmRecovery = async () => {
  const result = await commands.recoverNsfwPin(
    recoveryCodeInput.value,
    recoveryNewPin.value,
  );
  if (result.status !== "ok") {
    toastStore.reportError("Failed to recover PIN", result.error);
    return;
  }
  nsfwAccess.value = result.data.state;
  issuedRecoveryCode.value = result.data.recovery_code;
  cancelRecovery();
  await loadNsfwDesigners();
  toastStore.addToast("PIN reset — save the new recovery code", "success");
};

// Designer-wide rule: every model by a listed designer counts as 18+ unless
// it explicitly opts out (the drawer's per-model toggle). Backed by its own
// table, not settings — set_designer_nsfw/list_nsfw_designers commands.
const nsfwDesigners = ref<string[]>([]);
const loadNsfwDesigners = async () => {
  if (!nsfwAccess.value.unlocked) {
    nsfwDesigners.value = [];
    return;
  }
  const result = await commands.listNsfwDesigners();
  if (result.status === "ok") nsfwDesigners.value = result.data;
};
const newNsfwDesigner = ref("");
const addNsfwDesigner = async () => {
  const name = newNsfwDesigner.value.trim();
  newNsfwDesigner.value = "";
  if (!name) return;
  const result = await commands.setDesignerNsfw(name, true);
  if (result.status !== "ok") {
    toastStore.reportError("Failed to add designer", result.error);
    return;
  }
  await loadNsfwDesigners();
};
const removeNsfwDesigner = async (name: string) => {
  const result = await commands.setDesignerNsfw(name, false);
  if (result.status !== "ok") {
    toastStore.reportError("Failed to remove designer", result.error);
    return;
  }
  await loadNsfwDesigners();
};

// The tab is kept alive (KeepAlive in App.vue): onMounted fires once, but
// folders get hidden from the Catalog tab — refresh the list on every return
onActivated(() => {
  loadIgnoredFolders();
  syncNsfwAccess().then(loadNsfwDesigners);
});

onMounted(async () => {
  loadIgnoredFolders();
  await syncNsfwAccess();
  loadNsfwDesigners();
  try {
    const savedSettings = await commands.getSettings();
    if (savedSettings.status === "ok") {
      savedSettings.data.compression_type =
        savedSettings.data.compression_type || "Zip";
      settings.value = savedSettings.data;
      toastStore.addToast("Settings loaded successfully", "success", 3000);
    } else {
      toastStore.reportError("Failed to load settings", savedSettings.error);
    }
  } catch (error) {
    toastStore.reportError("Failed to load settings", error);
  } finally {
    // Enable auto-save only after the initial load has populated the form
    setTimeout(() => {
      settingsLoaded.value = true;
    }, 0);
  }
});

const saveSettings = async () => {
  try {
    // The Catalog tab owns the roots list (and the setup dialog owns the
    // Blender acknowledgement) and this tab may have loaded before they
    // wrote — saving the stale copy would drop their changes. Re-read the
    // authoritative values right before writing.
    const fresh = await commands.getSettings();
    const payload =
      fresh.status === "ok"
        ? {
            ...settings.value,
            catalog_root: fresh.data.catalog_root,
            catalog_roots: fresh.data.catalog_roots,
            catalog_primary_root: fresh.data.catalog_primary_root,
            blender_setup_acknowledged: fresh.data.blender_setup_acknowledged,
          }
        : settings.value;
    const result = await commands.setSettings(payload);
    if (result.status === "ok") {
      toastStore.addToast("Settings saved successfully", "success", 3000);
    }
    if (result.status === "error") {
      toastStore.reportError("Failed to save settings", result.error);
    }
  } catch (error) {
    toastStore.reportError("Error saving settings", error);
  }
};
</script>
