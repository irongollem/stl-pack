# stl-pack

stl-pack is an opinionated desktop tool for compressing and bundling STL files "the right way". It provides an easy-to-use interface for optimizing your 3D models, ensuring they are efficiently packed and ready for use.

## Features

- Compress STL files to reduce file size without losing quality.
- Bundle multiple STL files into a single package.
- User-friendly interface for easy operation.
- Supports batch processing of multiple files.
- Cross-platform support (Windows, macOS, Linux).

## Installation

To install stl-pack, download the latest release from the [releases page](https://github.com/irongollem/stl-pack/releases) and follow the installation instructions for your operating system.

## Usage

1. Open stl-pack.
2. Add STL files you want to compress or bundle.
3. Choose your desired compression settings.
4. Click the "Compress" or "Bundle" button.
5. Save the optimized files to your desired location.

## Recommended IDE Setup for Development

- [VS Code](https://code.visualstudio.com/) + [Volar](https://marketplace.visualstudio.com/items?itemName=Vue.volar) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)

## Type Support For `.vue` Imports in TS

Since TypeScript cannot handle type information for `.vue` imports, they are shimmed to be a generic Vue component type by default. In most cases this is fine if you don't really care about component prop types outside of templates. However, if you wish to get actual prop types in `.vue` imports (for example to get props validation when using manual `h(...)` calls), you can enable Volar's Take Over mode by following these steps:

1. Run `Extensions: Show Built-in Extensions` from VS Code's command palette, look for `TypeScript and JavaScript Language Features`, then right click and select `Disable (Workspace)`. By default, Take Over mode will enable itself if the default TypeScript extension is disabled.
2. Reload the VS Code window by running `Developer: Reload Window` from the command palette.

You can learn more about Take Over mode [here](https://github.com/johnsoncodehk/volar/discussions/471).

## Contributing

Contributions are welcome! Please read the [contributing guidelines](CONTRIBUTING.md) first.

## License

At this point we are unsure about the license. We will update this section once we have decided on a license. For now feel free to beta test the software and provide feedback.