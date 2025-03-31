# Contribution Guidelines

Thank you for considering contributing to stl-pack! We welcome all contributions, whether they are bug reports, feature requests, code changes, or documentation improvements.

## Code of Conduct

By participating in this project, you agree to maintain a respectful and inclusive environment for everyone. Please be kind and courteous in all interactions.

## Development Setup

1. **Fork and Clone the Repository**
   ```bash
   git clone https://github.com/YOUR_USERNAME/stl-pack.git
   cd stl-pack
   ```

2. **Install Dependencies**
   ```bash
   bun install
   ```

3. **Setup Tauri Development Environment**
   - Follow [Tauri's setup guide](https://tauri.app/v1/guides/getting-started/prerequisites) for your operating system
   - Ensure Rust and required dependencies are installed

4. **Run the Development Server**
   ```bash
   cargo tauri dev
   ```

## Reporting Issues

- Use the [GitHub issue tracker](https://github.com/irongollem/stl-pack/issues) to report bugs or suggest features
- Before creating a new issue, please check if a similar issue already exists
- Provide as much context as possible: version numbers, error messages, steps to reproduce, etc.

## Pull Request Process

1. **Create a Branch**
   ```bash
   git checkout -b feature/your-feature-name
   ```

2. **Make Your Changes**
   - Write clean, well-commented code
   - Follow existing code style and patterns
   - Include tests for new functionality

3. **Commit Your Changes**
   ```bash
   git commit -m "Brief description of changes"
   ```
   - Use clear and descriptive commit messages
   - Reference issue numbers when applicable

4. **Push to Your Fork**
   ```bash
   git push origin feature/your-feature-name
   ```

5. **Submit a Pull Request**
   - Provide a clear description of the changes
   - Link to any related issues
   - Be open to feedback and be prepared to make additional changes if requested

## Coding Standards

- Follow the existing code style of the project
- For TypeScript/JavaScript code:
  - Use 2-space indentation
  - Use semicolons
  - Follow Biome rules configured in the project
- For Rust code:
  - Follow the [Rust style guide](https://doc.rust-lang.org/1.0.0/style/README.html)
  - Run `bunx biome format --write` before committing

## Testing

- Add tests for new features or bug fixes
- Ensure all tests pass before submitting a pull request
- Run tests with `bun test`

## Documentation

- Update documentation for any changed functionality
- Document new features, options, and behaviors
- Use clear and concise language

## License Considerations

This project uses a custom source-available license. By contributing to this project, you agree that your contributions will be licensed under the same license. Please review the [license terms](LICENCE.md) before contributing.

## Questions?

If you have any questions about contributing, feel free to open an issue for clarification.

We appreciate your contributions to making stl-pack better!
