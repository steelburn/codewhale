## Summary

## Testing

<!-- Exact gate commands (including the clippy allow list) are in
     CONTRIBUTING.md → "Pre-push verification". -->

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --all-features --locked` (warning-free under the CI allow list)
- [ ] `cargo test --workspace --all-features --locked`

## Checklist

- [ ] Updated docs or comments as needed
- [ ] Added or updated tests where relevant
- [ ] Verified TUI behavior manually if UI changes
- [ ] Harvested/co-authored credit uses a GitHub numeric noreply address
