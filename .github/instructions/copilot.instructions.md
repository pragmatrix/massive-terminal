---
applyTo: '**'
---

## Rust

- Try to keep references short (At maximum one path component + `::` + item name identifier). Add `use` if needed. 
- Prefer to add `use` statements to the top of the file.
- Prefer not to abbreviate identifiers.
- Add function below their usage, not above.
- For new types, try to add derive(Debug) at least.