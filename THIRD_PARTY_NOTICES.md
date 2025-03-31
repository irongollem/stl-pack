# Third-Party Notices

This software includes components from the following third parties:

## 7-Zip

This software uses 7-Zip, which is licensed under multiple licenses:

### GNU LGPL
Most of the 7-Zip code is licensed under GNU LGPL. Note that this license requires:
- If you modify LGPL code, you must distribute the modified code under LGPL
- If you distribute binaries, you must provide source code or a written offer for it

### BSD 3-Clause License
Some parts of 7-Zip code are licensed under the BSD 3-clause License.

### unRAR License Restriction
The decompression engine for RAR archives was developed using source code of unRAR program.
All copyrights to original unRAR code are owned by Alexander Roshal.

The license for unRAR code restricts the development of RAR-compatible archivers and has the following restriction:

> The unRAR sources cannot be used to develop a RAR (WinRAR) compatible archiver.

Full 7-Zip License information: https://www.7-zip.org/license.txt

## Rust Crates
This application depends on various open-source Rust libraries, each with their own licenses.
Notable dependencies include:
- tauri: MIT License
- serde: MIT or Apache-2.0 License
- zip: MIT License
- image: MIT License

## JavaScript/TypeScript Dependencies
This application depends on various open-source JavaScript/TypeScript libraries, each with their own licenses.
Notable dependencies include:
- Vue.js: MIT License
- Pinia: MIT License
- Tailwind CSS: MIT License
- DaisyUI: MIT License

The full list of dependencies and their licenses can be viewed in the `package.json` and `Cargo.toml` files.