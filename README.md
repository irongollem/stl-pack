# stl-pack

[![GitHub Release](https://img.shields.io/github/v/release/irongollem/stl-pack?include_prereleases&style=flat-square)](https://github.com/irongollem/stl-pack/releases)
[![GitHub Issues](https://img.shields.io/github/issues/irongollem/stl-pack?style=flat-square)](https://github.com/irongollem/stl-pack/issues)
[![GitHub Stars](https://img.shields.io/github/stars/irongollem/stl-pack?style=flat-square&cacheSeconds=3600)](https://github.com/irongollem/stl-pack/stargazers)
[![GitHub Downloads](https://img.shields.io/github/downloads/irongollem/stl-pack/total?style=flat-square)](https://github.com/irongollem/stl-pack/releases)
[![Contributions Welcome](https://img.shields.io/badge/contributions-welcome-brightgreen.svg?style=flat-square)](CONTRIBUTING.md)
[![Built with Tauri](https://img.shields.io/badge/built%20with-Tauri-purple?style=flat-square)](https://tauri.app/)

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

This project is available under a custom source-available license that allows:
- Free usage for personal, educational, and commercial purposes
- Creating and selling content/bundles made with the software

But prohibits:
- Selling or redistributing the software itself
- Creating derivative works based on the software

See [`LICENCE.md`](LICENCE.md) for the complete license terms and [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md) for information about included components.
