---
applyTo: '**'
---

## General

- Preserve original comments when modifying code. Do not remove existing comments unless explicitly asked to.

## Rust

- Try to keep references short (At maximum one path component + `::` + item name identifier). Add `use` if needed. 
- Prefer to add `use` statements to the top of the file.
- Prefer not to abbreviate identifiers.
- **IMPORTANT: Add functions below their usage, not above.** Helper/private functions should appear after the public functions that call them.
- For new types, try to add derive(Debug) at least.
- Use `const` for identity values (e.g., `const IDENTITY: Self = ...`) instead of methods like `identity()`.
- When checking if values are at their default/identity state, prefer exact equality (`==`) over approximate comparisons, as these values are typically unchanged from construction or deliberately set.
- Extract repeated code patterns into reusable functions or methods to reduce duplication and improve maintainability.
