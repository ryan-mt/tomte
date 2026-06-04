```markdown
# tomte Development Patterns

> Auto-generated skill from repository analysis

## Overview
This skill teaches the core development patterns and conventions used in the `tomte` Rust repository. You'll learn about file organization, code style, commit message standards, and how to write and run tests in this codebase. The repository does not use a specific framework, focusing on idiomatic Rust with custom conventions.

## Coding Conventions

### File Naming
- **PascalCase** is used for file names.
  - Example: `MyModule.rs`, `UserProfile.rs`

### Import Style
- **Relative imports** are preferred.
  - Example:
    ```rust
    use super::MyModule;
    use crate::utils::Helper;
    ```

### Export Style
- **Named exports** are used to expose items.
  - Example:
    ```rust
    pub struct MyStruct { /* ... */ }
    pub fn my_function() { /* ... */ }
    ```

### Commit Messages
- **Conventional commit** format is used, with the `feat` prefix for features.
  - Example:
    ```
    feat: add user authentication module
    ```

## Workflows

### Creating a New Feature
**Trigger:** When adding a new feature to the codebase  
**Command:** `/new-feature`

1. Create a new file using PascalCase (e.g., `NewFeature.rs`).
2. Implement the feature using relative imports as needed.
3. Export structs, enums, or functions with `pub`.
4. Write a commit message starting with `feat:` and a concise description.
5. Push your changes and open a pull request.

### Writing and Running Tests
**Trigger:** When verifying the correctness of code  
**Command:** `/run-tests`

1. Create a test file matching the pattern `*.test.*` (e.g., `MyModule.test.rs`).
2. Write tests using Rust's built-in test framework (or as per project conventions).
3. Run tests using `cargo test` or the project's preferred test runner.
4. Review and fix any failing tests before merging.

## Testing Patterns

- Test files follow the pattern `*.test.*` (e.g., `Feature.test.rs`).
- The testing framework is not explicitly specified; use Rust's built-in test framework unless otherwise noted.
- Example test:
    ```rust
    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_feature() {
            assert_eq!(my_function(), expected_value);
        }
    }
    ```

## Commands
| Command         | Purpose                              |
|-----------------|--------------------------------------|
| /new-feature    | Start a new feature implementation   |
| /run-tests      | Run all tests in the repository      |
```
