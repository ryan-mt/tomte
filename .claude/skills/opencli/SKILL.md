```markdown
# opencli Development Patterns

> Auto-generated skill from repository analysis

## Overview
This skill teaches the core development patterns, coding conventions, and workflows used in the `opencli` Rust codebase. You'll learn how to structure files, write imports and exports, follow commit message guidelines, and understand the repository's testing patterns. This guide is ideal for contributors seeking to maintain consistency and quality in the `opencli` project.

## Coding Conventions

### File Naming
- Use **camelCase** for file names.
  - Example: `commandParser.rs`, `cliHandler.rs`

### Import Style
- Use **relative imports** within the codebase.
  - Example:
    ```rust
    mod utils;
    use crate::commandParser::parse_command;
    ```

### Export Style
- Use **named exports** to expose functions, structs, or modules.
  - Example:
    ```rust
    pub fn run_cli() { /* ... */ }
    pub struct CliOptions { /* ... */ }
    ```

### Commit Messages
- Follow the **conventional commit** format.
- Use the `fix` prefix for bug fixes.
  - Example:
    ```
    fix: handle edge case in command parsing for empty input
    ```
- Typical commit message length is around 83 characters.

## Workflows

### Making a Code Change
**Trigger:** When you need to fix a bug or add a new feature  
**Command:** `/make-change`

1. Create a new branch for your change.
2. Make code changes following the coding conventions.
3. Write or update tests as needed.
4. Commit your changes using the conventional commit format (e.g., `fix: ...`).
5. Push your branch and open a pull request.

### Adding a Test
**Trigger:** When adding new functionality or fixing a bug  
**Command:** `/add-test`

1. Create a test file following the `*.test.*` pattern (e.g., `commandParser.test.rs`).
2. Write tests for your new or modified code.
3. Run tests to ensure correctness.

### Importing and Exporting Modules
**Trigger:** When structuring or refactoring code  
**Command:** `/import-export`

1. Use relative imports for internal modules.
   ```rust
   use crate::utils::helper_function;
   ```
2. Export functions, structs, or modules using named exports.
   ```rust
   pub fn new_function() { /* ... */ }
   ```

## Testing Patterns

- Test files follow the `*.test.*` naming pattern (e.g., `cliHandler.test.rs`).
- The specific testing framework is not identified, but tests are likely written in Rust's built-in test framework.
- Example test structure:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn test_parse_command() {
          // Test implementation here
      }
  }
  ```

## Commands
| Command         | Purpose                                               |
|-----------------|-------------------------------------------------------|
| /make-change    | Step-by-step guide for making a code change           |
| /add-test       | Instructions for adding and running tests             |
| /import-export  | Guide for importing and exporting modules             |
```
