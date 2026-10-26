# Adding a cleanup category

A cleanup category is the only way MacClean can delete anything, so each new one is reviewed as a security change. **Open an issue first** (use the Feature request form), and agree on the details below before writing code.

## 1. Requirements for a category

A category is accepted only if you can answer all of these with evidence such as the tool's documentation or source code:

| Question | Example (`npm-cache`) |
|---|---|
| Where is the root, and how is it found without running the tool? | `npm_config_cache`, then `cache=` in `~/.npmrc`, then `~/.npm`. Then `_cacache` inside it. |
| Which direct children of the root are safe to delete? List them by exact name or a strict pattern. | `content-v2`, `index-v5`, `tmp` |
| What must never be touched, even though it lives nearby? | `_logs`, `_npx`, unknown entries |
| How does the tool recover after deletion? | npm downloads packages again when it needs them |
| When must cleaning be refused? | The layout isn't recognized. For Xcode: while Xcode is running. |
| Is the root always inside the home directory, and owned by the user? | Yes. Otherwise `validate_category_root` refuses it. |

Categories that need `sudo`, live outside the home directory, require running the tool itself, or hold user data (not just regenerable cache) will not be accepted.

## 2. Code changes

All in `src/cleaner.rs` unless noted:

1. **`CategoryId`**: add a variant with a doc comment, add it to `CategoryId::ALL`, and add `name()` (kebab-case, which is also the CLI value) and `description()` (what is deleted, how it comes back, and what is never touched).
2. **`resolve_root`**: return `Resolution::Root`, `NotPresent` or `Refused`. Always finish with `finish_resolution`, which canonicalizes the path and runs `policy::validate_category_root`. If the location is fixed, refuse when it resolves somewhere else, as `XcodeDerivedData` does.
3. **`classify`**: return `Verdict::safe` only for entries you positively recognize. Everything else gets `Verdict::review`. The ownership and symlink checks at the top of `classify` apply to every category, so keep them first.
4. **Refusal conditions**: if the tool must not be running, follow `xcode_is_running` and add an injectable override to `CleanContext` (like `xcode_running: Option<bool>`) so tests don't depend on real processes.
5. **No new deletion code.** Don't add new calls to `unlink_at`, `rmdir_at` or `std::fs::remove_*`. Deletion goes through the existing `Walker`, which already applies every skip rule.

## 3. Tests (required)

Add tests to `tests/cleaner.rs` using the `Fixture` fake home directory. At a minimum:

* a dry-run plan deletes nothing, and its estimate matches the allocated bytes
* `execute` deletes only the recognized entries, and unknown siblings survive
* the root is refused when it is a symlink, outside home, in a protected location, or replaced after planning
* a symlink inside a candidate is unlinked, and its target survives
* every refusal condition you defined (for example, tool running, or unrecognized layout)

`sh scripts/check.sh` must pass.

## 4. Documentation

* README: add a section under **Cleanup categories** (what it deletes, how the location is found, what it never touches), update the `clean --category` usage line, and add any new limitation to **Known limitations of the security model**.
* `CHANGELOG.md`: add an entry under **Unreleased → Added**.
